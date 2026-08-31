//! Self-hosted runners: pair a machine that phones home (no inbound dial).

use dioxus::prelude::*;
use fleet_runners_view::routes::NAV_LABEL_SELF_HOSTED;
use fleet_runners_view::{
    BEHAVIOR_LABEL, accept_jobs_label, accept_jobs_title, accepting_jobs, agent_status_phrase,
    catalog_provider_display_name, clamp_max_jobs, disk_free_line, display_name,
    draft_holding_new_jobs, extra_status_pills, format_bytes, format_ts, hint_is_loopback,
    identity_line, in_removed_list, is_control_plane, is_removed, jobs_line, last_seen_label,
    not_taking_reason, package_header_driver_line, package_header_host_line,
    provider_install_command, public_status, runtime_line, settings_draft_dirty, shows_auto_update,
    shows_software_line, software_bin_rows, sort_hosts_for_list, specs_line, stale_package_warning,
    status_label, update_now_label, update_requested, widgets_locked, worker_id_label,
};
use gloo_timers::future::TimeoutFuture;

use super::api::{
    CreateHostBody, CreateHostResponse, HostPatch, HostPublic, LocalModelCatalog, LocalModelTag,
    LocalModelTags, PackageInventory, create_host, fetch_local_model_tags, fetch_local_models,
    install_commands, list_workers_page, patch_host, revoke_host, windows_install_command,
};
use crate::clipboard::copy_text;
use crate::components::toast::show_toast;
use fleet_runners_view::InstallModelRequest;

const POLL_MS: u32 = 10_000;

fn field_class() -> &'static str {
    "rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-2 py-1 text-sm text-[var(--app-fg)] outline-none focus:border-[var(--app-muted)]"
}

fn status_pill_class(status: &str) -> &'static str {
    match status {
        "online" => "bg-emerald-500/15 text-emerald-400 border-emerald-500/30",
        "pending" | "pending apply" | "outdated" => {
            "bg-amber-500/15 text-amber-400 border-amber-500/30"
        }
        "offline" => "bg-[var(--app-bg-hover)] text-[var(--app-muted)] border-[var(--app-border)]",
        "disabled" => "bg-[var(--app-bg-hover)] text-[var(--app-muted)] border-[var(--app-border)]",
        "error" => "bg-red-500/15 text-red-400 border-red-500/30",
        _ => "bg-[var(--app-bg-hover)] text-[var(--app-muted)] border-[var(--app-border)]",
    }
}

fn replace_host(list: &mut Vec<HostPublic>, updated: HostPublic) {
    if let Some(slot) = list.iter_mut().find(|h| h.id == updated.id) {
        *slot = updated;
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
    let mut sandbox_backend = use_signal(|| "stub".to_string());
    let mut selected_id = use_signal(|| read_runner_query());

    // Do not read `hosts` / `loading` here — a write-triggered effect loops
    // and Firefox OOMs when the first card mounts.
    let ws_poll = workspace_id.clone();
    use_effect(move || {
        let ws = ws_poll.clone();
        spawn(async move {
            let mut first = true;
            loop {
                match list_workers_page(&ws).await {
                    Ok((list, inv, backend)) => {
                        hosts.set(list);
                        packages.set(inv);
                        if !backend.trim().is_empty() {
                            sandbox_backend.set(backend);
                        }
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
            match create_host(
                &ws,
                &CreateHostBody {
                    name: Some("New worker".into()),
                },
            )
            .await
            {
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
    sort_hosts_for_list(&mut active, js_sys::Date::now());
    let removed: Vec<HostPublic> = all
        .iter()
        .filter(|h| in_removed_list(h, &all))
        .cloned()
        .collect();
    let removed_n = removed.len();

    let pkg_ver = packages().package_versions;
    let stale_builds = stale_package_warning(&packages());

    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-3xl flex-col overflow-auto px-4 py-6 md:px-6",
            div { class: "flex flex-wrap items-start justify-between gap-3",
                div { class: "min-w-0 flex-1",
                    h1 { class: "m-0 text-2xl font-semibold tracking-tight text-[var(--app-fg)]",
                        "{NAV_LABEL_SELF_HOSTED}"
                    }
                    p { class: "mt-1 text-sm text-[var(--app-muted)]",
                        "Pair a machine as a self-hosted worker. It phones home; the plane never dials inbound. Jobs run as processes on that machine."
                    }
                    if let Some(host_line) = package_header_host_line(&pkg_ver) {
                        p { class: "mt-2 text-xs text-[var(--app-muted)]",
                            title: "Verification version ([verify] in runner-packages.toml). Same number on every OS.",
                            "{host_line}"
                        }
                    }
                    if let Some(driver_line) = package_header_driver_line(&pkg_ver) {
                        p { class: "mt-0.5 text-xs text-[var(--app-muted)]",
                            title: "Verification version ([verify] in runner-packages.toml). Same number on every OS.",
                            "{driver_line}"
                        }
                    }
                    if let Some(warn) = stale_builds {
                        p { class: "mt-2 text-xs text-amber-400",
                            title: "Stage every OS: scripts/stage-worker-dist.sh --all",
                            "{warn}"
                        }
                    }
                }
                button {
                    class: "rounded-lg border border-[var(--app-border)] bg-[var(--app-bg-elevated)] px-3 py-2 text-sm font-medium text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)] disabled:opacity-50",
                    r#type: "button",
                    disabled: creating(),
                    onclick: on_add,
                    if creating() { "Adding…" } else { "Add worker" }
                }
            }

            if let Some(created) = pair_result() {
                PairPanel {
                    workspace_id: workspace_id.clone(),
                    created,
                    on_dismiss: move |_| pair_result.set(None),
                    on_revoked: move |id: String| {
                        let mut list = hosts();
                        if let Some(slot) = list.iter_mut().find(|h| h.id == id) {
                            slot.status = "disabled".into();
                        }
                        hosts.set(list);
                        pair_result.set(None);
                    },
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
                                HostDetailPanel {
                                    key: "{host.id}",
                                    workspace_id: workspace_id.clone(),
                                    sandbox_backend: sandbox_backend(),
                                    host: host.clone(),
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
                                        write_runner_query(None);
                                        selected_id.set(None);
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
                                    HostListRow {
                                        key: "rm-{host.id}",
                                        sandbox_backend: sandbox_backend(),
                                        selected: selected_id().as_deref() == Some(host.id.as_str()),
                                        host: host.clone(),
                                        on_open: move |id: String| {
                                            write_runner_query(Some(&id));
                                            selected_id.set(Some(id));
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

fn read_runner_query() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        let search = web_sys::window()?.location().search().ok()?;
        let q = search.trim_start_matches('?');
        for pair in q.split('&') {
            let mut it = pair.splitn(2, '=');
            if it.next() == Some("runner") {
                let raw = it.next().unwrap_or("");
                if !raw.is_empty() {
                    return Some(raw.to_string());
                }
            }
        }
        return None;
    }
    #[cfg(not(target_arch = "wasm32"))]
    None
}

fn write_runner_query(id: Option<&str>) {
    #[cfg(target_arch = "wasm32")]
    {
        let Some(window) = web_sys::window() else {
            return;
        };
        let Ok(path) = window.location().pathname() else {
            return;
        };
        let url = match id {
            Some(id) => format!("{path}?runner={id}"),
            None => path,
        };
        if let Ok(history) = window.history() {
            let _ = history.replace_state_with_url(&wasm_bindgen::JsValue::NULL, "", Some(&url));
        }
    }
    let _ = id;
}

#[component]
fn PairPanel(
    workspace_id: String,
    created: CreateHostResponse,
    on_dismiss: EventHandler<MouseEvent>,
    on_revoked: EventHandler<String>,
) -> Element {
    let mut cancelling = use_signal(|| false);
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
                "On the machine, open a terminal and paste the command. It installs fleet-host and fleet-driver, then pairs and starts."
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

            div { class: "mt-4 flex flex-wrap items-center gap-2",
                button {
                    class: "rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-1.5 text-sm font-medium text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)] disabled:opacity-50",
                    r#type: "button",
                    disabled: cancelling(),
                    onclick: move |e| on_dismiss.call(e),
                    "Done"
                }
                button {
                    class: "rounded-lg border border-[var(--app-border)] px-3 py-1.5 text-sm font-medium text-[var(--app-muted)] hover:bg-[var(--app-bg-hover)] hover:text-[var(--app-fg)] disabled:opacity-50",
                    r#type: "button",
                    disabled: cancelling(),
                    onclick: {
                        let workspace_id = workspace_id.clone();
                        let host_id = created.host.id.clone();
                        move |_| {
                            if cancelling() {
                                return;
                            }
                            cancelling.set(true);
                            let ws = workspace_id.clone();
                            let id = host_id.clone();
                            spawn(async move {
                                match revoke_host(&ws, &id).await {
                                    Ok(()) => {
                                        on_revoked.call(id);
                                        show_toast().success("Worker discarded.");
                                    }
                                    Err(e) => {
                                        show_toast().error(e);
                                        cancelling.set(false);
                                    }
                                }
                            });
                        }
                    },
                    if cancelling() { "Cancelling…" } else { "Cancel" }
                }
            }
        }
    }
}

fn btn_class() -> &'static str {
    "rounded-lg border border-[var(--app-border)] px-2.5 py-1.5 text-xs font-medium text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)] disabled:opacity-50"
}

#[component]
fn HostListRow(
    sandbox_backend: String,
    selected: bool,
    host: HostPublic,
    on_open: EventHandler<String>,
) -> Element {
    let extra_pills = extra_status_pills(&host);
    let runtime = runtime_line(&host, &sandbox_backend);
    let now_ms = js_sys::Date::now();
    let status = public_status(&host, now_ms);
    let pill = status_pill_class(status);
    let last_seen = last_seen_label(&host, now_ms);
    let title = {
        let n = host.name.trim();
        if n.is_empty() {
            "Unnamed worker".to_string()
        } else {
            n.to_string()
        }
    };
    let name_tags_label = {
        let mut tags: Vec<&str> = Vec::new();
        if host.is_control_plane_box {
            tags.push("control plane");
        }
        if host.is_this_machine {
            tags.push("this machine");
        }
        if tags.is_empty() {
            None
        } else {
            Some(tags.join(" · "))
        }
    };
    let plane = is_control_plane(&host);
    let ring = if selected {
        "border-[var(--app-fg)]/40"
    } else {
        "border-[var(--app-border)]"
    };
    let host_id = host.id.clone();
    rsx! {
        article {
            class: "w-full cursor-pointer rounded-2xl border {ring} bg-[var(--app-bg-elevated)] p-4 text-left hover:bg-[var(--app-bg-hover)]",
            onclick: move |_| on_open.call(host_id.clone()),
            div { class: "flex flex-wrap items-start justify-between gap-x-3 gap-y-1",
                div { class: "min-w-0 flex flex-wrap items-center gap-2",
                    span { class: "text-sm font-semibold text-[var(--app-fg)]",
                        "{title}"
                    }
                    if let Some(tags) = &name_tags_label {
                        span { class: "text-xs text-[var(--app-muted)]",
                            "({tags})"
                        }
                    }
                    span {
                        class: "inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-medium {pill}",
                        "{status_label(status)}"
                    }
                    for extra in extra_pills {
                        span {
                            class: "inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-medium {status_pill_class(extra)}",
                            "{extra}"
                        }
                    }
                }
                div { class: "flex flex-wrap items-center gap-x-3 text-xs text-[var(--app-muted)]",
                    span { if plane { "{last_seen}" } else { "last seen {last_seen}" } }
                    span { "id {worker_id_label(&host)}" }
                }
            }
            div { class: "mt-3 space-y-1 text-xs text-[var(--app-muted)]",
                if let Some(identity) = identity_line(&host) {
                    p { class: "m-0", "{identity}" }
                }
                if let Some(specs) = specs_line(&host) {
                    p { class: "m-0", "{specs}" }
                }
                p { class: "m-0", "{jobs_line(&host)}" }
                p { class: "m-0", "{runtime}" }
                if plane {
                    p { class: "m-0",
                        "This process does not run sandboxes."
                    }
                }
            }
        }
    }
}

#[component]
fn HostDetailPanel(
    workspace_id: String,
    sandbox_backend: String,
    host: HostPublic,
    on_updated: EventHandler<HostPublic>,
    on_revoked: EventHandler<String>,
) -> Element {
    let mut busy = use_signal(|| false);
    let mut confirm_remove = use_signal(|| false);
    let mut renaming = use_signal(|| false);
    let mut name_draft = use_signal(|| host.name.clone());
    let mut d_accepting = use_signal(|| accepting_jobs(&host) && !host.paused && !host.drain);
    let mut d_auto = use_signal(|| host.auto_update);
    let mut d_max = use_signal(|| host.max_concurrent);
    let mut d_allow_host = use_signal(|| host.allow_host);
    let mut d_allow_container = use_signal(|| host.allow_container);
    let mut d_timeout = use_signal(|| host.max_job_timeout_s);
    let mut d_work_dir = use_signal(|| filled_work_dir(&host));
    let mut d_models_dir = use_signal(|| filled_models_dir(&host));
    let mut d_backend = use_signal(|| filled_runtime(&host, "container_backend", "k3d"));
    let mut d_image = use_signal(|| filled_runtime(&host, "docker_image", "agent-fleet-api:local"));
    let mut d_network = use_signal(|| filled_runtime(&host, "docker_network", "bridge"));
    let mut d_namespace = use_signal(|| filled_runtime(&host, "k8s_namespace", "agent-fleet"));
    let mut d_k3d_cluster = use_signal(|| filled_runtime(&host, "k3d_cluster", "agent-fleet"));
    let mut d_k3d_memory = use_signal(|| filled_runtime(&host, "k3d_memory", "4g"));
    let mut d_k3d_agents = use_signal(|| {
        runtime_value(&host, "k3d_agents")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
    });
    let mut d_privileged = use_signal(|| runtime_bool(&host, "allow_privileged", false));
    let mut d_cpu = use_signal(|| filled_runtime(&host, "k8s_cpu", "250m"));
    let mut d_memory = use_signal(|| filled_runtime(&host, "k8s_memory", "256Mi"));
    let mut d_ephemeral = use_signal(|| filled_runtime(&host, "k8s_ephemeral", "1Gi"));
    let mut d_egress = use_signal(|| filled_runtime(&host, "egress_mode", "none"));
    let plane = is_control_plane(&host);
    let sandbox_dirty = !plane
        && (d_allow_host() != host.allow_host
            || d_allow_container() != host.allow_container
            || d_timeout() != host.max_job_timeout_s
            || d_work_dir() != filled_work_dir(&host)
            || d_models_dir() != filled_models_dir(&host)
            || d_backend() != filled_runtime(&host, "container_backend", "k3d")
            || d_image() != filled_runtime(&host, "docker_image", "agent-fleet-api:local")
            || d_network() != filled_runtime(&host, "docker_network", "bridge")
            || d_namespace() != filled_runtime(&host, "k8s_namespace", "agent-fleet")
            || d_k3d_cluster() != filled_runtime(&host, "k3d_cluster", "agent-fleet")
            || d_k3d_memory() != filled_runtime(&host, "k3d_memory", "4g")
            || d_k3d_agents()
                != runtime_value(&host, "k3d_agents")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0)
            || d_privileged() != runtime_bool(&host, "allow_privileged", false)
            || d_cpu() != filled_runtime(&host, "k8s_cpu", "250m")
            || d_memory() != filled_runtime(&host, "k8s_memory", "256Mi")
            || d_ephemeral() != filled_runtime(&host, "k8s_ephemeral", "1Gi")
            || d_egress() != filled_runtime(&host, "egress_mode", "none"));
    let dirty = settings_draft_dirty(&host, d_accepting(), d_auto(), d_max()) || sandbox_dirty;
    let pending = widgets_locked(&host, false);
    let extra_pills = extra_status_pills(&host);
    let draft_holding = draft_holding_new_jobs(d_accepting());
    let runtime = runtime_line(&host, &sandbox_backend);
    let revoked = host.status == "disabled";
    let now_ms = js_sys::Date::now();
    let status = public_status(&host, now_ms);
    let pill = status_pill_class(status);
    let last_seen = last_seen_label(&host, now_ms);
    let title = {
        let n = host.name.trim();
        if n.is_empty() {
            "Unnamed worker".to_string()
        } else {
            n.to_string()
        }
    };
    let name_tags_label = {
        let mut tags: Vec<&str> = Vec::new();
        if host.is_control_plane_box {
            tags.push("control plane");
        }
        if host.is_this_machine {
            tags.push("this machine");
        }
        if tags.is_empty() {
            None
        } else {
            Some(tags.join(" · "))
        }
    };
    let why = not_taking_reason(&host);
    let last_error = host.last_error.clone().unwrap_or_default();
    let show_error = !last_error.is_empty()
        && why
            .as_ref()
            .map(|w| !w.contains(&last_error))
            .unwrap_or(true);

    rsx! {
        article { class: "flex min-h-0 flex-1 flex-col",
            div { class: "min-h-0 flex-1 space-y-3 overflow-y-auto px-4 py-3",
            div { class: "flex flex-wrap items-start justify-between gap-x-3 gap-y-1",
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
                    if let Some(tags) = &name_tags_label {
                        span { class: "text-xs text-[var(--app-muted)]",
                            "({tags})"
                        }
                    }
                    span {
                        class: "inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-medium {pill}",
                        "{status_label(status)}"
                    }
                    for extra in extra_pills {
                        span {
                            class: "inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-medium {status_pill_class(extra)}",
                            "{extra}"
                        }
                    }
                }
                div { class: "flex flex-wrap items-center gap-x-3 text-xs text-[var(--app-muted)]",
                    span { if plane { "{last_seen}" } else { "last seen {last_seen}" } }
                    span {
                        title: "Stable worker id. Same machine keeps this id after remove + re-pair.",
                        "id {worker_id_label(&host)}"
                    }
                }
            }

            div { class: "mt-3 space-y-1 text-xs text-[var(--app-muted)]",
                if let Some(identity) = identity_line(&host) {
                    p { class: "m-0", "{identity}" }
                }
                if let Some(specs) = specs_line(&host) {
                    p { class: "m-0", "{specs}" }
                }
                if shows_software_line(&host) {
                    for row in software_bin_rows(&host) {
                        p {
                            class: if row.status == "behind" || row.status == "missing" {
                                "m-0 text-amber-400"
                            } else {
                                "m-0"
                            },
                            title: "{row.name} package version this box is running",
                            "{row.name} {row.version} · {row.status}"
                        }
                    }
                }
                p { class: "m-0",
                    "{jobs_line(&host)}"
                }
                p { class: "m-0",
                    "{runtime}"
                }
                if plane {
                    p { class: "m-0",
                        "This process does not run sandboxes."
                    }
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
                div { class: "mt-3 space-y-3 border-t border-[var(--app-border)] pt-3",
                    div { class: "flex flex-col gap-1",
                    span {
                        class: "text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)]",
                        "{BEHAVIOR_LABEL}"
                    }
                    div { class: "flex flex-wrap items-center gap-x-4 gap-y-2",
                    button {
                        class: "{btn_class()}",
                        r#type: "button",
                        disabled: busy(),
                        title: "{accept_jobs_title(draft_holding)}",
                        onclick: move |_| {
                            d_accepting.set(draft_holding);
                        },
                        "{accept_jobs_label(draft_holding)}"
                    }
                    if shows_auto_update(&host) {
                    label {
                        class: "inline-flex items-center gap-1.5 text-xs text-[var(--app-fg)]",
                        title: "When on, this worker pulls the served package by itself. Turn off to wait for Update now.",
                        input {
                            r#type: "checkbox",
                            checked: d_auto(),
                            disabled: busy() || pending,
                            onchange: move |e| d_auto.set(e.checked()),
                        }
                        "Auto-update"
                    }
                    }
                    if let Some(upd_label) = update_now_label(&host, d_auto()) {
                        button {
                            class: "{btn_class()}",
                            r#type: "button",
                            disabled: busy() || update_requested(&host),
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
                            "{upd_label}"
                        }
                    }
                    label { class: "inline-flex items-center gap-1.5 text-xs text-[var(--app-fg)]",
                        "Max jobs at once"
                        input {
                            class: "{field_class()} w-16",
                            r#type: "number",
                            min: "1",
                            max: "256",
                            value: "{d_max()}",
                            disabled: busy() || pending,
                            onchange: move |e| {
                                if let Ok(n) = e.value().parse::<i32>() {
                                    d_max.set(clamp_max_jobs(n));
                                }
                            },
                        }
                    }
                    }
                    }
                }

                AgentLoginBlock {
                    workspace_id: workspace_id.clone(),
                    host: host.clone(),
                    models_dir: d_models_dir,
                    on_updated,
                }

                if !plane && host.status != "pending" {
                    SandboxBlock {
                        host: host.clone(),
                        pending,
                        work_dir: d_work_dir,
                        allow_host: d_allow_host,
                        allow_container: d_allow_container,
                        timeout: d_timeout,
                        backend: d_backend,
                        image: d_image,
                        network: d_network,
                        namespace: d_namespace,
                        k3d_cluster: d_k3d_cluster,
                        k3d_memory: d_k3d_memory,
                        k3d_agents: d_k3d_agents,
                        privileged: d_privileged,
                        cpu: d_cpu,
                        memory: d_memory,
                        ephemeral: d_ephemeral,
                        egress: d_egress,
                    }
                }

                div { class: "mt-3 flex flex-wrap items-center justify-between gap-2 border-t border-[var(--app-border)] pt-3",
                    if plane {
                        span {}
                    } else if confirm_remove() {
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
            if dirty && !revoked {
                div { class: "shrink-0 border-t border-[var(--app-border)] bg-[var(--app-bg-elevated)] px-4 py-2",
                    button {
                        class: "w-full rounded-lg border border-emerald-500/30 px-2.5 py-1.5 text-xs font-semibold text-emerald-300 hover:bg-emerald-500/15 disabled:opacity-50",
                        r#type: "button",
                        disabled: busy(),
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
                                let patch = if plane {
                                    HostPatch {
                                        allow_host: Some(d_accepting()),
                                        allow_container: Some(d_accepting()),
                                        max_concurrent: Some(d_max()),
                                        paused: Some(false),
                                        drain: Some(false),
                                        ..HostPatch::default()
                                    }
                                } else {
                                    HostPatch {
                                        allow_host: Some(d_allow_host()),
                                        allow_container: Some(d_allow_container()),
                                        auto_update: Some(d_auto()),
                                        max_concurrent: Some(d_max()),
                                        max_job_timeout_s: Some(d_timeout()),
                                        paused: Some(!d_accepting()),
                                        drain: Some(false),
                                        runtime_settings: Some(serde_json::json!({
                                            "work_dir": d_work_dir(),
                                            "ollama_models_dir": d_models_dir(),
                                            "run_host": d_allow_host(),
                                            "run_container": d_allow_container(),
                                            "container_backend": d_backend(),
                                            "docker_image": d_image(),
                                            "docker_network": d_network(),
                                            "k8s_namespace": d_namespace(),
                                            "k3d_cluster": d_k3d_cluster(),
                                            "k3d_memory": d_k3d_memory(),
                                            "k3d_agents": d_k3d_agents(),
                                            "allow_privileged": d_privileged(),
                                            "k8s_cpu": d_cpu(),
                                            "k8s_memory": d_memory(),
                                            "k8s_ephemeral": d_ephemeral(),
                                            "egress_mode": d_egress(),
                                        })),
                                        ..HostPatch::default()
                                    }
                                };
                                spawn(async move {
                                    match patch_host(&ws, &current.id, &current, &patch).await {
                                        Ok(updated) => {
                                            on_updated.call(updated);
                                            show_toast().success(if plane {
                                                "Settings applied."
                                            } else {
                                                "Settings sent. Waiting for the runner."
                                            });
                                        }
                                        Err(e) => show_toast().error(e),
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                        "Apply changes"
                    }
                }
            }
        }
    }
}

const CATALOG_AGENT_IDS: &[&str] = &["grok", "claude", "openai", "ollama"];

#[component]
fn AgentLoginBlock(
    workspace_id: String,
    host: HostPublic,
    models_dir: Signal<String>,
    on_updated: EventHandler<HostPublic>,
) -> Element {
    let empty_inventory = host.agents.is_empty();
    let disk = disk_free_line(&host);
    let pending_install = host
        .install_request
        .as_ref()
        .and_then(|v| v.get("spec"))
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());
    rsx! {
        div { class: "space-y-2 border-t border-[var(--app-border)] pt-3",
            span {
                class: "text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)]",
                "Providers on this runner"
            }
            if let Some(disk) = disk {
                p { class: "m-0 text-[11px] text-[var(--app-muted)]",
                    "Disk {disk}"
                }
            }
            if empty_inventory {
                p { class: "m-0 text-xs text-[var(--app-muted)]",
                    "No heartbeat inventory yet (old fleet-host). Restart fleet-host from this tree. Until then, expected providers:"
                }
            }
            ul { class: "m-0 list-none space-y-2 p-0",
                for id in CATALOG_AGENT_IDS {
                    {
                        let reported = host.agents.iter().find(|a| a.id == *id);
                        let installed = reported.map(|a| a.installed).unwrap_or(false);
                        let logged_in = reported.map(|a| a.logged_in).unwrap_or(false);
                        let models = reported.map(|a| a.models.clone()).unwrap_or_default();
                        let uses_login = *id != "ollama";
                        let cmd = provider_install_command(id, host.platform.as_deref());
                        let name = catalog_provider_display_name(id);
                        rsx! {
                            li { class: "text-xs text-[var(--app-fg)]",
                                div { class: "flex items-center justify-between gap-2",
                                    span { class: "font-medium", "{name}" }
                                    span {
                                        class: if installed { "text-emerald-400" } else { "text-[var(--app-muted)]" },
                                        "{agent_status_phrase(id, installed, logged_in)}"
                                    }
                                }
                                if !installed {
                                    div { class: "mt-0.5 flex flex-wrap items-center gap-1",
                                        code { class: "min-w-0 flex-1 break-all text-[11px] text-[var(--app-muted)]",
                                            "{cmd}"
                                        }
                                        button {
                                            class: "{btn_class()}",
                                            r#type: "button",
                                            onclick: {
                                                let cmd = cmd.clone();
                                                move |_| {
                                                    if copy_text(&cmd) {
                                                        show_toast().success("Copied install command. Run it on that runner.");
                                                    } else {
                                                        show_toast().error(format!("Run on that runner: {cmd}"));
                                                    }
                                                }
                                            },
                                            "Copy"
                                        }
                                    }
                                } else if uses_login && !logged_in {
                                    p { class: "m-0 mt-0.5 font-mono text-[11px] text-[var(--app-muted)]",
                                        {login_hint(id)}
                                    }
                                }
                                if !models.is_empty() {
                                    p { class: "m-0 mt-0.5 text-[11px] text-[var(--app-muted)]",
                                        "Installed: {models.join(\", \")}"
                                    }
                                }
                                if *id == "ollama" && installed {
                                    OllamaInstallPicker {
                                        workspace_id: workspace_id.clone(),
                                        host: host.clone(),
                                        models_dir,
                                        installed_models: models.clone(),
                                        pending_install: pending_install.clone(),
                                        on_updated,
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

fn login_hint(id: &str) -> String {
    match id {
        "grok" => "On this machine: grok login".into(),
        "claude" => "On this machine: claude login".into(),
        "openai" => "On this machine: set OPENAI_API_KEY or sign in with the official CLI".into(),
        "ollama" => "Start Ollama on this machine".into(),
        other => format!("Log in to {other} on this machine"),
    }
}

fn runtime_value<'a>(host: &'a HostPublic, key: &str) -> Option<&'a serde_json::Value> {
    host.runtime_reported
        .as_ref()
        .and_then(|v| v.get(key))
        .or_else(|| host.runtime_settings.as_ref().and_then(|v| v.get(key)))
}

fn runtime_str(host: &HostPublic, key: &str) -> String {
    runtime_value(host, key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn runtime_bool(host: &HostPublic, key: &str, fallback: bool) -> bool {
    runtime_value(host, key)
        .and_then(|v| v.as_bool())
        .unwrap_or(fallback)
}

fn host_is_windows(host: &HostPublic) -> bool {
    host.platform
        .as_deref()
        .is_some_and(|p| p.to_ascii_lowercase().starts_with("windows"))
}

fn default_work_dir(host: &HostPublic) -> String {
    if host_is_windows(host) {
        "%LOCALAPPDATA%\\fleet-host\\work".into()
    } else {
        "~/.local/share/fleet-host/work".into()
    }
}

fn default_ollama_models_dir(host: &HostPublic) -> String {
    if host_is_windows(host) {
        "%USERPROFILE%\\.ollama\\models".into()
    } else {
        "~/.ollama/models".into()
    }
}

fn filled_runtime(host: &HostPublic, key: &str, fallback: &str) -> String {
    let s = runtime_str(host, key);
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

fn filled_models_dir(host: &HostPublic) -> String {
    filled_runtime(host, "ollama_models_dir", &default_ollama_models_dir(host))
}

fn filled_work_dir(host: &HostPublic) -> String {
    filled_runtime(host, "work_dir", &default_work_dir(host))
}

fn host_disk_free(host: &HostPublic) -> Option<u64> {
    host.hardware.as_ref().and_then(|h| h.disk_free_bytes)
}

fn host_ram(host: &HostPublic) -> Option<u64> {
    host.hardware.as_ref().and_then(|h| h.mem_total_bytes)
}

fn host_max_vram(host: &HostPublic) -> Option<u64> {
    host.hardware
        .as_ref()?
        .gpus
        .as_ref()?
        .iter()
        .filter_map(|g| g.vram_bytes)
        .max()
}

fn model_on_runner(installed: &[String], slug: &str) -> bool {
    let prefix = format!("{slug}:");
    installed
        .iter()
        .any(|m| m == slug || m.starts_with(&prefix))
}

#[component]
fn OllamaInstallPicker(
    workspace_id: String,
    host: HostPublic,
    mut models_dir: Signal<String>,
    installed_models: Vec<String>,
    pending_install: Option<String>,
    on_updated: EventHandler<HostPublic>,
) -> Element {
    let mut pull_draft = use_signal(|| String::new());
    let mut filter = use_signal(|| String::new());
    let mut installing = use_signal(|| false);
    let mut catalog = use_signal(|| None::<LocalModelCatalog>);
    let mut tags = use_signal(|| None::<LocalModelTags>);
    let mut tags_loading = use_signal(|| false);
    let mut selected_slug = use_signal(|| String::new());
    let mut confirm = use_signal(|| None::<(String, String)>);

    let ws_cat = workspace_id.clone();
    use_future(move || {
        let ws = ws_cat.clone();
        async move {
            match fetch_local_models(&ws).await {
                Ok(c) => catalog.set(Some(c)),
                Err(_) => catalog.set(Some(LocalModelCatalog {
                    models: Vec::new(),
                    source: "unavailable".into(),
                })),
            }
        }
    });

    let q = filter().trim().to_ascii_lowercase();
    let cat = catalog();
    let mut rows = cat.as_ref().map(|c| c.models.clone()).unwrap_or_default();
    rows.sort_by_key(|m| !model_on_runner(&installed_models, &m.name));
    if !q.is_empty() {
        rows.retain(|m| {
            m.name.to_ascii_lowercase().contains(&q)
                || m.summary
                    .as_deref()
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .contains(&q)
        });
    }
    let catalog_ready = cat.is_some();
    let catalog_empty = cat.as_ref().is_some_and(|c| c.models.is_empty());
    let catalog_source = cat.as_ref().map(|c| c.source.as_str()).unwrap_or_default();
    let draft = pull_draft();

    rsx! {
        if let Some(pending) = pending_install.clone() {
            p { class: "m-0 mt-1 text-[11px] text-amber-300/90",
                "Installing {pending} on the runner…"
            }
        }
        div { class: "mt-1 space-y-1",
            span {
                class: "text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)]",
                "Popular models"
            }
            if !catalog_ready {
                p { class: "m-0 text-[11px] text-[var(--app-muted)]",
                    "Loading current top 50…"
                }
            } else if catalog_empty {
                p { class: "m-0 text-[11px] text-[var(--app-muted)]",
                    if catalog_source == "unavailable" {
                        "Popular list unavailable — type an unlisted name below."
                    } else {
                        "No popular models returned — type an unlisted name below."
                    }
                }
            } else {
                input {
                    class: "{field_class()} w-full",
                    r#type: "search",
                    placeholder: "Filter the current top 50",
                    value: "{filter}",
                    oninput: move |e| filter.set(e.value()),
                }
                ul { class: "m-0 max-h-36 list-none space-y-0.5 overflow-y-auto rounded-lg border border-[var(--app-border)] p-1",
                    if rows.is_empty() {
                        li { class: "px-2 py-1 text-[11px] text-[var(--app-muted)]",
                            "No listed match."
                        }
                    }
                    for m in rows.iter() {
                        {
                            let name = m.name.clone();
                            let summary = m.summary.clone().unwrap_or_default();
                            let on_runner = model_on_runner(&installed_models, &name);
                            let selected = draft.trim() == name;
                            let pick = name.clone();
                            let slug_selected = selected_slug() == name;
                            rsx! {
                                li {
                                    button {
                                        class: if selected || slug_selected {
                                            "flex w-full flex-col items-start rounded-md bg-[var(--app-bg-hover)] px-2 py-1 text-left"
                                        } else {
                                            "flex w-full flex-col items-start rounded-md px-2 py-1 text-left hover:bg-[var(--app-bg-hover)]"
                                        },
                                        r#type: "button",
                                        onclick: {
                                            let workspace_id = workspace_id.clone();
                                            let pick = pick.clone();
                                            move |_| {
                                                pull_draft.set(pick.clone());
                                                selected_slug.set(pick.clone());
                                                tags.set(None);
                                                tags_loading.set(true);
                                                let ws = workspace_id.clone();
                                                let name = pick.clone();
                                                spawn(async move {
                                                    match fetch_local_model_tags(&ws, &name).await {
                                                        Ok(t) => tags.set(Some(t)),
                                                        Err(_) => tags.set(Some(LocalModelTags {
                                                            name,
                                                            tags: Vec::new(),
                                                            source: "unavailable".into(),
                                                        })),
                                                    }
                                                    tags_loading.set(false);
                                                });
                                            }
                                        },
                                        span { class: "flex w-full items-center justify-between gap-2",
                                            span { class: "font-mono text-[11px] text-[var(--app-fg)]",
                                                "{name}"
                                            }
                                            if on_runner {
                                                span { class: "shrink-0 text-[10px] text-emerald-400",
                                                    "on this runner"
                                                }
                                            }
                                        }
                                        if !summary.is_empty() {
                                            span { class: "text-[10px] text-[var(--app-muted)]",
                                                "{summary}"
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
        if !selected_slug().is_empty() {
            div { class: "mt-1 space-y-1",
                span {
                    class: "text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)]",
                    "Tags for {selected_slug}"
                }
                if tags_loading() {
                    p { class: "m-0 text-[11px] text-[var(--app-muted)]", "Loading tags…" }
                } else if let Some(tg) = tags() {
                    if tg.tags.is_empty() {
                        p { class: "m-0 text-[11px] text-[var(--app-muted)]",
                            if tg.source == "unavailable" {
                                "Tag list unavailable — type name:tag in Unlisted."
                            } else {
                                "No tag list — type name:tag in Unlisted."
                            }
                        }
                    } else {
                        ul { class: "m-0 max-h-32 list-none space-y-0.5 overflow-y-auto rounded-lg border border-[var(--app-border)] p-1",
                            for t in tg.tags.iter() {
                                {
                                    let spec = if t.spec.is_empty() {
                                        format!("{}:{}", t.name, t.tag)
                                    } else {
                                        t.spec.clone()
                                    };
                                    let size = t.size_bytes;
                                    let ram = t.ram_bytes;
                                    let latest = t.latest;
                                    let tag = t.tag.clone();
                                    let selected = pull_draft().trim() == spec;
                                    rsx! {
                                        li {
                                            button {
                                                class: if selected {
                                                    "flex w-full items-center justify-between gap-2 rounded-md bg-[var(--app-bg-hover)] px-2 py-1 text-left"
                                                } else {
                                                    "flex w-full items-center justify-between gap-2 rounded-md px-2 py-1 text-left hover:bg-[var(--app-bg-hover)]"
                                                },
                                                r#type: "button",
                                                onclick: move |_| pull_draft.set(spec.clone()),
                                                span { class: "font-mono text-[11px]",
                                                    "{tag}"
                                                    if latest {
                                                        span { class: "ml-1 text-[10px] text-emerald-400", "latest" }
                                                    }
                                                }
                                                span { class: "text-[10px] text-[var(--app-muted)]",
                                                    {
                                                        let mut bits = Vec::new();
                                                        if let Some(b) = size {
                                                            bits.push(format_bytes(b));
                                                        }
                                                        if let Some(b) = ram {
                                                            bits.push(format!("{} RAM", format_bytes(b)));
                                                        }
                                                        bits.join(" · ")
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
            }
        }
        div { class: "mt-1 space-y-1",
            span {
                class: "text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)]",
                "Models directory"
            }
            p { class: "m-0 text-[11px] text-[var(--app-muted)]",
                "Where Ollama installs and looks for weights (`OLLAMA_MODELS`)."
            }
            SelectOrCustom {
                value: models_dir,
                presets: ollama_dir_presets(&host),
                custom_placeholder: default_ollama_models_dir(&host),
                disabled: installing(),
            }
        }
        div { class: "mt-1 space-y-1",
            span {
                class: "text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)]",
                "Unlisted"
            }
            div { class: "flex flex-wrap items-center gap-1",
                input {
                    class: "{field_class()} min-w-[8rem] flex-1",
                    placeholder: "name or URL (llama3.2, hf.co/…)",
                    value: "{pull_draft}",
                    oninput: move |e| pull_draft.set(e.value()),
                }
                button {
                    class: "{btn_class()}",
                    r#type: "button",
                    disabled: installing() || pending_install.is_some(),
                    onclick: {
                        let workspace_id = workspace_id.clone();
                        let host = host.clone();
                        move |_| {
                            let spec = pull_draft().trim().to_string();
                            if spec.is_empty() {
                                return;
                            }
                            let tag = tags().and_then(|tg| {
                                tg.tags.into_iter().find(|t| {
                                    t.spec == spec || format!("{}:{}", t.name, t.tag) == spec
                                })
                            });
                            let size = tag.as_ref().and_then(|t| t.size_bytes);
                            let ram_need = tag.as_ref().and_then(|t| t.ram_bytes);
                            if let (Some(need), Some(free)) = (size, host_disk_free(&host)) {
                                if need > free {
                                    show_toast().error(format!(
                                        "Not enough disk: model is {}, runner has {} free. Will not install.",
                                        format_bytes(need),
                                        format_bytes(free)
                                    ));
                                    return;
                                }
                            }
                            if let (Some(need), Some(ram)) = (ram_need, host_ram(&host)) {
                                if need > ram {
                                    confirm.set(Some((
                                        spec.clone(),
                                        format!(
                                            "This tag wants {} RAM; this machine has {}. Proceed anyway?",
                                            format_bytes(need),
                                            format_bytes(ram)
                                        ),
                                    )));
                                    return;
                                }
                            }
                            if let (Some(need), Some(vram)) = (size, host_max_vram(&host)) {
                                if vram >= 4 * 1000 * 1000 * 1000 && need > vram {
                                    confirm.set(Some((
                                        spec.clone(),
                                        format!(
                                            "GPU VRAM is {} and this tag is {}. It will likely spill to system RAM and run slower. Proceed anyway?",
                                            format_bytes(vram),
                                            format_bytes(need)
                                        ),
                                    )));
                                    return;
                                }
                            }
                            queue_ollama_install(
                                workspace_id.clone(),
                                host.clone(),
                                spec,
                                installing,
                                on_updated,
                            );
                        }
                    },
                    "Install on runner"
                }
            }
            if let Some((spec, msg)) = confirm() {
                div { class: "rounded-lg border border-amber-500/30 bg-amber-500/10 px-2 py-1.5 text-[11px] text-amber-200",
                    p { class: "m-0", "{msg}" }
                    div { class: "mt-1 flex gap-1",
                        button {
                            class: "{btn_class()}",
                            r#type: "button",
                            onclick: {
                                let workspace_id = workspace_id.clone();
                                let host = host.clone();
                                move |_| {
                                    confirm.set(None);
                                    queue_ollama_install(
                                        workspace_id.clone(),
                                        host.clone(),
                                        spec.clone(),
                                        installing,
                                        on_updated,
                                    );
                                }
                            },
                            "Proceed anyway"
                        }
                        button {
                            class: "{btn_class()}",
                            r#type: "button",
                            onclick: move |_| confirm.set(None),
                            "Cancel"
                        }
                    }
                }
            }
            p { class: "m-0 text-[11px] text-[var(--app-muted)]",
                "Pick a listed model, choose a tag, or type any Ollama name / pull URL. We do not pin versions."
            }
        }
    }
}

fn queue_ollama_install(
    workspace_id: String,
    host: HostPublic,
    spec: String,
    mut installing: Signal<bool>,
    on_updated: EventHandler<HostPublic>,
) {
    installing.set(true);
    let patch = HostPatch {
        install_model: Some(InstallModelRequest {
            provider: "ollama".into(),
            spec: spec.clone(),
        }),
        ..HostPatch::default()
    };
    spawn(async move {
        match patch_host(&workspace_id, &host.id, &host, &patch).await {
            Ok(updated) => {
                on_updated.call(updated);
                show_toast().success(format!("Queued `{spec}` on the runner."));
            }
            Err(e) => show_toast().error(e),
        }
        installing.set(false);
    });
}

fn ollama_dir_presets(host: &HostPublic) -> Vec<(String, String)> {
    if host_is_windows(host) {
        vec![
            (
                "%USERPROFILE%\\.ollama\\models".into(),
                "User profile (default)".into(),
            ),
            (
                "%LOCALAPPDATA%\\ollama\\models".into(),
                "Local AppData".into(),
            ),
        ]
    } else {
        vec![
            ("~/.ollama/models".into(), "Home (default)".into()),
            (
                "/usr/share/ollama/.ollama/models".into(),
                "System share".into(),
            ),
        ]
    }
}

fn work_dir_presets(host: &HostPublic) -> Vec<(String, String)> {
    if host_is_windows(host) {
        vec![(
            "%LOCALAPPDATA%\\fleet-host\\work".into(),
            "Local AppData (default)".into(),
        )]
    } else {
        vec![
            (
                "~/.local/share/fleet-host/work".into(),
                "User data (default)".into(),
            ),
            ("/var/lib/fleet-host/work".into(), "System /var".into()),
        ]
    }
}

#[component]
fn SelectOrCustom(
    mut value: Signal<String>,
    presets: Vec<(String, String)>,
    custom_placeholder: String,
    disabled: bool,
) -> Element {
    let current = value();
    let known: Vec<String> = presets.iter().map(|(v, _)| v.clone()).collect();
    let is_custom = !known.iter().any(|v| v == &current);
    let select_val = if is_custom {
        "__custom__".to_string()
    } else {
        current.clone()
    };
    rsx! {
        select {
            class: "{field_class()} w-full",
            value: "{select_val}",
            disabled,
            onchange: move |e| {
                let v = e.value();
                if v == "__custom__" {
                    if !is_custom {
                        value.set(String::new());
                    }
                } else {
                    value.set(v);
                }
            },
            for (v, label) in presets.iter() {
                option { value: "{v}", "{label}" }
            }
            option { value: "__custom__", "Custom…" }
        }
        if is_custom {
            input {
                class: "{field_class()} mt-0.5 w-full",
                value: "{value}",
                placeholder: "{custom_placeholder}",
                disabled,
                oninput: move |e| value.set(e.value()),
            }
        }
    }
}

#[component]
fn SandboxBlock(
    host: HostPublic,
    pending: bool,
    work_dir: Signal<String>,
    allow_host: Signal<bool>,
    allow_container: Signal<bool>,
    timeout: Signal<i32>,
    backend: Signal<String>,
    image: Signal<String>,
    network: Signal<String>,
    namespace: Signal<String>,
    k3d_cluster: Signal<String>,
    k3d_memory: Signal<String>,
    k3d_agents: Signal<i64>,
    privileged: Signal<bool>,
    cpu: Signal<String>,
    memory: Signal<String>,
    ephemeral: Signal<String>,
    egress: Signal<String>,
) -> Element {
    let mut work_dir = work_dir;
    let mut allow_host = allow_host;
    let mut allow_container = allow_container;
    let mut timeout = timeout;
    let mut backend = backend;
    let mut privileged = privileged;
    let mut k3d_agents = k3d_agents;
    let mut advanced = use_signal(|| false);
    rsx! {
        div { class: "mt-3 space-y-3 border-t border-[var(--app-border)] pt-3",
            span {
                class: "text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)]",
                "Sandboxing"
            }
            p { class: "m-0 text-[11px] text-[var(--app-muted)]",
                "What this runner may execute. Workspace Settings still set the minimum isolation (process < container < vm)."
            }

            div { class: "space-y-1.5 rounded-lg border border-[var(--app-border)] p-2",
                p { class: "m-0 text-xs font-medium text-[var(--app-fg)]", "Native" }
                p { class: "m-0 text-[11px] text-[var(--app-muted)]",
                    "Process jobs on this box. Root is the work directory; each task is a folder named with the session id and the official CLI runs inside it."
                }
                label { class: "flex items-center gap-1.5 text-xs",
                    input {
                        r#type: "checkbox",
                        checked: allow_host(),
                        disabled: pending,
                        onchange: move |e| allow_host.set(e.checked()),
                    }
                    "Allow process jobs"
                    span { class: "text-[10px] text-[var(--app-muted)]",
                        if host.advertised_host { "(box can run them)" } else { "(box is not advertising process)" }
                    }
                }
                label { class: "block text-[11px] text-[var(--app-muted)]",
                    "Work root"
                    div { class: "mt-0.5",
                        SelectOrCustom {
                            value: work_dir,
                            presets: work_dir_presets(&host),
                            custom_placeholder: default_work_dir(&host),
                            disabled: pending,
                        }
                    }
                }
                p { class: "m-0 font-mono text-[10px] text-[var(--app-muted)]",
                    "{work_dir}/{{session-id}}/"
                }
                label { class: "inline-flex items-center gap-1.5 text-xs",
                    "Max job timeout (s)"
                    input {
                        class: "{field_class()} w-20",
                        r#type: "number",
                        min: "60",
                        value: "{timeout}",
                        disabled: pending,
                        onchange: move |e| {
                            if let Ok(n) = e.value().parse::<i32>() {
                                timeout.set(n.max(60));
                            }
                        },
                    }
                }
            }

            div { class: "space-y-1.5 rounded-lg border border-[var(--app-border)] p-2",
                p { class: "m-0 text-xs font-medium text-[var(--app-fg)]", "VMs" }
                p { class: "m-0 text-[11px] text-[var(--app-muted)]",
                    "Whole-VM guests (Firecracker / QEMU / KubeVirt) are not assignable yet. Tick will not place a vm job on this runner. This section is a placeholder until that executor ships."
                }
            }

            div { class: "space-y-1.5 rounded-lg border border-[var(--app-border)] p-2",
                p { class: "m-0 text-xs font-medium text-[var(--app-fg)]", "Containers" }
                p { class: "m-0 text-[11px] text-[var(--app-muted)]",
                    "Docker on the box, or a FleetSandbox CR in Kubernetes. Missing tools fail closed — no process fallback."
                }
                label { class: "flex items-center gap-1.5 text-xs",
                    input {
                        r#type: "checkbox",
                        checked: allow_container(),
                        disabled: pending,
                        onchange: move |e| allow_container.set(e.checked()),
                    }
                    "Allow container jobs"
                    span { class: "text-[10px] text-[var(--app-muted)]",
                        if host.advertised_container { "(box can run them)" } else { "(box is not advertising container / k8s)" }
                    }
                }
                label { class: "block text-[11px] text-[var(--app-muted)]",
                    "How to run"
                    select {
                        class: "{field_class()} mt-0.5 w-full",
                        value: "{backend}",
                        disabled: pending,
                        onchange: move |e| backend.set(e.value()),
                        option { value: "k3d", "Kubernetes (k3d / kubeconfig / in-cluster)" }
                        option { value: "docker", "Docker on this box" }
                    }
                }
                label { class: "block text-[11px] text-[var(--app-muted)]",
                    "Worker image"
                    div { class: "mt-0.5",
                        SelectOrCustom {
                            value: image,
                            presets: vec![
                                ("agent-fleet-api:local".into(), "agent-fleet-api:local".into()),
                                ("agent-fleet-api:latest".into(), "agent-fleet-api:latest".into()),
                            ],
                            custom_placeholder: "registry/image:tag",
                            disabled: pending,
                        }
                    }
                }
                label { class: "block text-[11px] text-[var(--app-muted)]",
                    "Pod network"
                    select {
                        class: "{field_class()} mt-0.5 w-full",
                        value: "{egress}",
                        disabled: pending,
                        onchange: move |e| egress.set(e.value()),
                        option { value: "none", "DNS + control plane only" }
                        option { value: "dns_only", "DNS only" }
                        option { value: "allowlist", "Also allow the worker API address" }
                    }
                }
                button {
                    class: "text-[11px] text-[var(--app-muted)] underline-offset-2 hover:text-[var(--app-fg)] hover:underline",
                    r#type: "button",
                    onclick: move |_| advanced.set(!advanced()),
                    if advanced() { "Hide advanced" } else { "Advanced…" }
                }
                if advanced() {
                    label { class: "block text-[11px] text-[var(--app-muted)]",
                        "Docker network"
                        div { class: "mt-0.5",
                            SelectOrCustom {
                                value: network,
                                presets: vec![
                                    ("bridge".into(), "bridge".into()),
                                    ("host".into(), "host".into()),
                                    ("none".into(), "none".into()),
                                ],
                                custom_placeholder: "network name",
                                disabled: pending,
                            }
                        }
                    }
                    label { class: "block text-[11px] text-[var(--app-muted)]",
                        "Kubernetes namespace"
                        div { class: "mt-0.5",
                            SelectOrCustom {
                                value: namespace,
                                presets: vec![
                                    ("agent-fleet".into(), "agent-fleet".into()),
                                    ("default".into(), "default".into()),
                                ],
                                custom_placeholder: "namespace",
                                disabled: pending,
                            }
                        }
                    }
                    label { class: "block text-[11px] text-[var(--app-muted)]",
                        "CPU request"
                        div { class: "mt-0.5",
                            SelectOrCustom {
                                value: cpu,
                                presets: vec![
                                    ("250m".into(), "250m".into()),
                                    ("500m".into(), "500m".into()),
                                    ("1".into(), "1".into()),
                                    ("2".into(), "2".into()),
                                ],
                                custom_placeholder: "e.g. 750m",
                                disabled: pending,
                            }
                        }
                    }
                    label { class: "block text-[11px] text-[var(--app-muted)]",
                        "Memory request"
                        div { class: "mt-0.5",
                            SelectOrCustom {
                                value: memory,
                                presets: vec![
                                    ("256Mi".into(), "256Mi".into()),
                                    ("512Mi".into(), "512Mi".into()),
                                    ("1Gi".into(), "1Gi".into()),
                                    ("2Gi".into(), "2Gi".into()),
                                    ("4Gi".into(), "4Gi".into()),
                                ],
                                custom_placeholder: "e.g. 1536Mi",
                                disabled: pending,
                            }
                        }
                    }
                    label { class: "block text-[11px] text-[var(--app-muted)]",
                        "Ephemeral storage"
                        div { class: "mt-0.5",
                            SelectOrCustom {
                                value: ephemeral,
                                presets: vec![
                                    ("1Gi".into(), "1Gi".into()),
                                    ("2Gi".into(), "2Gi".into()),
                                    ("5Gi".into(), "5Gi".into()),
                                    ("10Gi".into(), "10Gi".into()),
                                ],
                                custom_placeholder: "e.g. 8Gi",
                                disabled: pending,
                            }
                        }
                    }
                    label { class: "block text-[11px] text-[var(--app-muted)]",
                        "k3d cluster"
                        div { class: "mt-0.5",
                            SelectOrCustom {
                                value: k3d_cluster,
                                presets: vec![("agent-fleet".into(), "agent-fleet".into())],
                                custom_placeholder: "cluster name",
                                disabled: pending,
                            }
                        }
                    }
                    label { class: "block text-[11px] text-[var(--app-muted)]",
                        "k3d node memory"
                        div { class: "mt-0.5",
                            SelectOrCustom {
                                value: k3d_memory,
                                presets: vec![
                                    ("2g".into(), "2g".into()),
                                    ("4g".into(), "4g".into()),
                                    ("8g".into(), "8g".into()),
                                    ("16g".into(), "16g".into()),
                                ],
                                custom_placeholder: "e.g. 6g",
                                disabled: pending,
                            }
                        }
                    }
                    label { class: "inline-flex items-center gap-1.5 text-xs",
                        "k3d agents"
                        input {
                            class: "{field_class()} w-14",
                            r#type: "number",
                            min: "0",
                            max: "16",
                            value: "{k3d_agents}",
                            disabled: pending,
                            onchange: move |e| {
                                if let Ok(n) = e.value().parse::<i64>() {
                                    k3d_agents.set(n.clamp(0, 16));
                                }
                            },
                        }
                    }
                    label { class: "flex items-center gap-1.5 text-xs",
                        input {
                            r#type: "checkbox",
                            checked: privileged(),
                            disabled: pending,
                            onchange: move |e| privileged.set(e.checked()),
                        }
                        "Allow privileged containers"
                    }
                    p { class: "m-0 text-[10px] text-[var(--app-muted)]",
                        "Pods cannot reach a localhost control-plane URL. Pair this runner with the LAN worker URL."
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    fn production_src() -> &'static str {
        include_str!("page.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("self_hosted page source")
    }

    fn pair_panel_src(prod: &str) -> &str {
        let idx = prod.find("fn PairPanel").expect("PairPanel");
        let after = &prod[idx..];
        let end = after[1..]
            .find("#[component]")
            .map(|i| i + 1)
            .unwrap_or(after.len());
        &after[..end]
    }

    #[test]
    fn pending_host_hides_sandbox_block() {
        let prod = production_src();
        assert!(
            prod.contains("if !plane && host.status != \"pending\""),
            "unpaired pending cards must not show Allow container / sandbox knobs"
        );
    }

    #[test]
    fn add_worker_sends_a_name() {
        let prod = production_src();
        assert!(
            prod.contains("name: Some(\"New worker\".into())"),
            "Add worker must POST a name; API 400s on omitted/empty"
        );
        assert!(
            !prod.contains("CreateHostBody::default()"),
            "default create body omits name and the plane rejects it"
        );
    }

    #[test]
    fn self_hosted_sorts_with_js_date_not_chrono_utc_now() {
        let prod = production_src();
        assert!(
            prod.contains("sort_hosts_for_list(&mut active, js_sys::Date::now())"),
            "Self-hosted must sort with Date.now(); chrono Utc::now panics on wasm32"
        );
        assert!(
            !prod.contains("chrono::Utc::now()"),
            "chrono::Utc::now() is unreachable in the UI wasm without wasmbind"
        );
    }

    /// Pair panel Copy buttons must call `copy_text` (textarea/execCommand
    /// first — HTTP LAN has no clipboard getter). A silent no-op or
    /// clipboard-API-only onclick still looks like a working button on
    /// localhost and dies on http://192.168.x.x.
    #[test]
    fn pair_panel_copy_buttons_call_copy_text() {
        let prod = production_src();
        let pair = pair_panel_src(prod);
        assert!(
            prod.contains("use crate::clipboard::copy_text"),
            "pair panel must use the HTTP LAN clipboard helper, not a local no-op"
        );
        assert!(
            !prod.contains("fn copy_text"),
            "page.rs must not shadow copy_text"
        );
        assert!(
            pair.contains("let unix = install_commands(&created)"),
            "Unix pre + Copy must be the pair install-worker.sh command"
        );
        assert!(
            pair.contains("let windows = windows_install_command(&created)"),
            "Windows pre + Copy must be the pair install-worker.ps1 command"
        );
        assert!(
            pair.contains("let unix_copy = unix.clone()"),
            "Unix Copy must copy the same command shown in the pre"
        );
        assert!(
            pair.contains("let windows_copy = windows.clone()"),
            "Windows Copy must copy the same command shown in the pre"
        );
        assert!(
            pair.contains("let token = created.pair_token.clone()"),
            "token Copy must copy the flp_ pair token shown once"
        );
        assert!(
            pair.contains("if copy_text(&unix_copy)"),
            "Unix Copy must call copy_text with the install-worker.sh command"
        );
        assert!(
            pair.contains("if copy_text(&windows_copy)"),
            "Windows Copy must call copy_text with the install-worker.ps1 command"
        );
        assert!(
            pair.contains("if copy_text(&token)"),
            "token Copy must call copy_text with the flp_ pair token"
        );
        assert_copy_gated_on_helper(pair, "if copy_text(&unix_copy)", "Command copied.");
        assert_copy_gated_on_helper(pair, "if copy_text(&windows_copy)", "Command copied.");
        assert_copy_gated_on_helper(pair, "if copy_text(&token)", "Token copied.");
        assert!(
            pair.contains("\"Copy failed. Select the command and copy it yourself.\""),
            "failed Unix/Windows copy must not look like success"
        );
        assert!(
            pair.contains("\"Copy failed. Select the token and copy it yourself.\""),
            "failed token copy must not look like success"
        );
        assert!(
            pair.matches("\"Copy\"").count() >= 3,
            "Pair this worker must offer Unix, Windows, and token Copy"
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
            !pair.contains("write_text"),
            "PairPanel must not call clipboard write_text itself"
        );
        assert!(
            pair.contains("{unix}")
                && pair.contains("{windows}")
                && pair.contains("{created.pair_token}"),
            "pres must show the same unix / windows / token values Copy sends"
        );
        assert_copy_is_type_button(pair, "if copy_text(&unix_copy)");
        assert_copy_is_type_button(pair, "if copy_text(&windows_copy)");
        assert_copy_is_type_button(pair, "if copy_text(&token)");

        // just test excludes fleet-ui; this crate still compiles the helper.
        // ExecCommand must come first so http://192.168.x.x never reads the
        // clipboard getter (Firefox throws; a silent no-op looks like Copy worked).
        let helper = include_str!("../../../clipboard.rs");
        let helper_prod = helper
            .split("#[cfg(test)]")
            .next()
            .expect("clipboard.rs production source");
        let lan = helper_prod
            .split("const HTTP_LAN_THEN_API")
            .nth(1)
            .and_then(|rest| rest.split(';').next())
            .expect("HTTP_LAN_THEN_API backends");
        let exec_at = lan
            .find("ExecCommandTextarea")
            .expect("textarea/execCommand backend");
        let api_at = lan
            .find("ClipboardApiIfPresent")
            .expect("clipboard API backend");
        assert!(
            exec_at < api_at,
            "HTTP LAN must try execCommand before navigator.clipboard"
        );
        assert!(
            helper_prod.contains("exec_command(\"copy\")"),
            "copy_text must actually execCommand('copy') the textarea"
        );
        assert!(
            helper_prod.contains("js_sys::Reflect::get"),
            "clipboard API probe must use Reflect.get, not the panic getter"
        );
        assert!(
            !helper_prod.contains("navigator().clipboard()"),
            "copy_text must not read navigator.clipboard (Firefox HTTP LAN panics)"
        );
        assert!(
            helper_prod.contains("if backends.is_empty()") && helper_prod.contains("return false"),
            "empty payload must fail closed, not toast success"
        );
        assert!(
            !pair.contains("is_secure_context") && !helper_prod.contains("is_secure_context"),
            "must not skip Copy on insecure HTTP LAN"
        );
    }

    /// Catalog: Done keeps the pending worker, closes the panel. Must not
    /// revoke/DELETE. Cancel is the discard path (sibling card).
    #[test]
    fn done_closes_pair_panel_without_revoke_keeps_pending() {
        let prod = production_src();
        let pair = pair_panel_src(prod);
        let done = button_named(pair, "\"Done\"");
        let cancel = button_named(
            pair,
            "if cancelling() { \"Cancelling…\" } else { \"Cancel\" }",
        );

        assert!(
            done.contains("onclick: move |e| on_dismiss.call(e)"),
            "Done must only dismiss the pair overlay:\n{done}"
        );
        assert!(
            !done.contains("revoke_host")
                && !done.contains("on_revoked")
                && !done.contains("spawn(")
                && !done.contains("DELETE")
                && !done.contains("authorized_delete"),
            "Done must not revoke or DELETE the unused pending host:\n{done}"
        );
        assert!(
            done.contains("disabled: cancelling()"),
            "Done stays a type=button while Cancel is in flight:\n{done}"
        );
        assert!(
            cancel.contains("revoke_host(&ws, &id)") && cancel.contains("on_revoked.call(id)"),
            "Cancel is the HTTP DELETE discard path — not Done:\n{cancel}"
        );
        assert!(
            cancel.contains("\"Worker discarded.\""),
            "Cancel toast must not be the Done path:\n{cancel}"
        );

        assert!(
            prod.contains("if let Some(created) = pair_result()")
                && prod.contains("on_dismiss: move |_| pair_result.set(None),"),
            "Pair this worker is shown only while pair_result is Some; Done clears it"
        );
        let dismiss_line = "on_dismiss: move |_| pair_result.set(None),";
        assert!(
            prod.contains(dismiss_line),
            "on_dismiss must only clear pair_result"
        );
        assert!(
            !dismiss_line.contains("revoke_host") && !dismiss_line.contains("on_revoked"),
            "on_dismiss must not revoke"
        );
        assert!(
            prod.contains("list.insert(0, created.host.clone())")
                && prod.contains("pair_result.set(Some(created))"),
            "Add worker must keep the pending host on the list before Done"
        );
        let revoked = prod
            .split("on_revoked: move |id: String|")
            .nth(1)
            .and_then(|rest| rest.split("PairPanel").next())
            .expect("on_revoked wiring");
        assert!(
            revoked.contains("slot.status = \"disabled\".into()")
                && revoked.contains("pair_result.set(None)"),
            "only Cancel/revoke marks the card disabled: {revoked}"
        );
        assert!(
            prod.contains("filter(|h| !is_removed(h))"),
            "unused pending (not disabled) stays in the active list after Done"
        );
    }

    fn button_named<'a>(src: &'a str, label: &str) -> &'a str {
        let at = src.find(label).unwrap_or_else(|| panic!("missing {label}"));
        let before = &src[..at];
        let start = before
            .rfind("button {")
            .unwrap_or_else(|| panic!("{label} must sit on a button {{ ... }}"));
        let after = &src[at + label.len()..];
        let end = after.find("button {").unwrap_or(after.len());
        &src[start..at + label.len() + end]
    }

    fn assert_copy_gated_on_helper(pair: &str, call: &str, ok_toast: &str) {
        let at = pair.find(call).unwrap_or_else(|| panic!("{call}"));
        let after = &pair[at..];
        let ok_at = after
            .find(ok_toast)
            .unwrap_or_else(|| panic!("{call} must toast {ok_toast}"));
        let err_at = after
            .find("Copy failed.")
            .unwrap_or_else(|| panic!("{call} must have a Copy failed path"));
        assert!(
            ok_at < err_at,
            "{call} must toast success only on copy_text ok, then the fail path"
        );
        assert!(
            !after[..ok_at].contains("let _ ="),
            "{call} must not ignore copy_text before toasting success"
        );
    }

    fn assert_copy_is_type_button(pair: &str, call: &str) {
        let at = pair.find(call).unwrap_or_else(|| panic!("{call}"));
        let before = &pair[..at];
        let start = before
            .rfind("button {")
            .unwrap_or_else(|| panic!("{call} must sit on a button {{ ... }} (not a submit form)"));
        assert!(
            before[start..].contains("r#type: \"button\""),
            "{call} must be type=button so HTTP LAN click stays a user gesture"
        );
    }
}
