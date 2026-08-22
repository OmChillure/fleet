//! Self-hosted runners: pair a machine that phones home (no inbound dial).

use dioxus::prelude::*;
use fleet_runners_view::routes::NAV_LABEL_SELF_HOSTED;
use fleet_runners_view::{
    BEHAVIOR_LABEL, accept_jobs_label, accept_jobs_title, accepting_jobs, clamp_max_jobs,
    display_name, draft_holding_new_jobs, extra_status_pills, format_ts, hint_is_loopback,
    identity_line, in_removed_list, is_control_plane, is_removed, jobs_line, last_seen_label,
    not_taking_reason, package_report, runtime_line, settings_draft_dirty, shows_software_line,
    software_flag, software_line, software_title, sort_hosts_for_list, specs_line, status_label,
    update_now_label, update_requested, widgets_locked, worker_id_label,
};
use gloo_timers::future::TimeoutFuture;

use super::api::{
    CreateHostBody, CreateHostResponse, HostPatch, HostPublic, PackageInventory, create_host,
    install_commands, list_workers_page, patch_host, revoke_host, windows_install_command,
};
use crate::clipboard::copy_text;
use crate::components::toast::show_toast;

const POLL_MS: u32 = 10_000;
const CURRENT_RUNNER_BUILD: &str = "Current runner build";

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
    sort_hosts_for_list(&mut active);
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
                        "{NAV_LABEL_SELF_HOSTED}"
                    }
                    p { class: "mt-1 text-sm text-[var(--app-muted)]",
                        "Pair a machine as a self-hosted worker. It phones home; the plane never dials inbound. Jobs run as processes on that machine."
                    }
                    if let Some(report) = package_report(&packages()) {
                        p { class: "mt-2 text-xs text-[var(--app-muted)]",
                            "{CURRENT_RUNNER_BUILD} {report.current_build}"
                        }
                        p { class: "mt-0.5 text-xs text-[var(--app-muted)]",
                            "Serving this build: {report.serving}"
                        }
                        for group in report.older {
                            p { class: "mt-0.5 text-xs text-amber-300/90",
                                "Outdated packages: {group}"
                            }
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
                                HostCard {
                                    key: "{host.id}",
                                    workspace_id: workspace_id.clone(),
                                    sandbox_backend: sandbox_backend(),
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
                                        sandbox_backend: sandbox_backend(),
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
fn HostCard(
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
    let mut d_accepting = use_signal(|| accepting_jobs(&host));
    let mut d_auto = use_signal(|| host.auto_update);
    let mut d_max = use_signal(|| host.max_concurrent);
    let plane = is_control_plane(&host);
    let dirty = settings_draft_dirty(&host, d_accepting(), d_auto(), d_max());
    let pending = widgets_locked(&host, false);
    let extra_pills = extra_status_pills(&host);
    let draft_holding = draft_holding_new_jobs(d_accepting());
    let runtime = runtime_line(&host, &sandbox_backend);
    let revoked = host.status == "disabled";
    let pill = status_pill_class(&host.status);
    let last_seen = last_seen_label(&host, js_sys::Date::now());
    let title = display_name(&host);
    let why = not_taking_reason(&host);
    let last_error = host.last_error.clone().unwrap_or_default();
    let show_error = !last_error.is_empty()
        && why
            .as_ref()
            .map(|w| !w.contains(&last_error))
            .unwrap_or(true);

    rsx! {
        article { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
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
                    span {
                        class: "inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-medium {pill}",
                        "{status_label(&host.status)}"
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
                    p {
                        class: "m-0",
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
                p { class: "m-0",
                    "{jobs_line(&host)}"
                }
                p { class: "m-0",
                    "{runtime}"
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
                    if !plane {
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
                div { class: "mt-3 flex flex-wrap items-center justify-between gap-2 border-t border-[var(--app-border)] pt-3",
                    if plane {
                        if dirty {
                            button {
                                class: "rounded-lg border border-emerald-500/30 px-2.5 py-1 text-xs font-semibold text-emerald-300 hover:bg-emerald-500/15 disabled:opacity-50",
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
                                        let patch = HostPatch {
                                            allow_host: Some(d_accepting()),
                                            allow_container: Some(d_accepting()),
                                            max_concurrent: Some(d_max()),
                                            paused: Some(false),
                                            drain: Some(false),
                                            ..HostPatch::default()
                                        };
                                        spawn(async move {
                                            match patch_host(&ws, &current.id, &current, &patch)
                                                .await
                                            {
                                                Ok(updated) => {
                                                    on_updated.call(updated);
                                                    show_toast().success("Settings applied.");
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
                        div { class: "flex flex-wrap items-center gap-2",
                            button {
                                class: "rounded-lg border border-red-500/30 px-2.5 py-1 text-xs font-semibold text-red-300 hover:bg-red-500/15 disabled:opacity-50",
                                r#type: "button",
                                disabled: busy(),
                                onclick: move |_| confirm_remove.set(true),
                                "Remove worker"
                            }
                            if dirty {
                                button {
                                    class: "rounded-lg border border-emerald-500/30 px-2.5 py-1 text-xs font-semibold text-emerald-300 hover:bg-emerald-500/15 disabled:opacity-50",
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
                                            let patch = HostPatch {
                                                allow_host: Some(d_accepting()),
                                                allow_container: Some(d_accepting()),
                                                auto_update: Some(d_auto()),
                                                max_concurrent: Some(d_max()),
                                                paused: Some(false),
                                                drain: Some(false),
                                                ..HostPatch::default()
                                            };
                                            spawn(async move {
                                                match patch_host(&ws, &current.id, &current, &patch)
                                                    .await
                                                {
                                                    Ok(updated) => {
                                                        on_updated.call(updated);
                                                        show_toast().success(
                                                            "Settings sent. Waiting for the runner.",
                                                        );
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
        }
    }
}
