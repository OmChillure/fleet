//! Workspace Workers: pair a machine that phones home (no inbound dial).

use dioxus::prelude::*;

use super::api::{
    CreateHostBody, CreateHostResponse, HostPatch, HostPublic, create_host, install_commands,
    list_hosts, patch_host, revoke_host,
};
use crate::components::toast::show_toast;

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

fn format_ts_opt(ts: &Option<String>) -> String {
    match ts {
        Some(s) if !s.is_empty() => format_ts(s),
        _ => "—".into(),
    }
}

fn hint_is_loopback(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("127.0.0.1") || lower.contains("localhost") || lower.contains("[::1]")
}

fn copy_text(text: &str) {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(window) = web_sys::window() {
            let clipboard = window.navigator().clipboard();
            let _ = clipboard.write_text(text);
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = text;
    }
}

fn replace_host(list: &mut Vec<HostPublic>, updated: HostPublic) {
    if let Some(slot) = list.iter_mut().find(|h| h.id == updated.id) {
        *slot = updated;
    }
}

#[component]
pub fn Workers(workspace_id: String) -> Element {
    let mut loading = use_signal(|| true);
    let mut creating = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut hosts = use_signal(Vec::<HostPublic>::new);
    let mut pair_result = use_signal(|| None::<CreateHostResponse>);

    let ws_load = workspace_id.clone();
    use_effect(move || {
        let ws = ws_load.clone();
        spawn(async move {
            match list_hosts(&ws).await {
                Ok(list) => {
                    hosts.set(list);
                    error.set(None);
                }
                Err(e) => {
                    error.set(Some(e.clone()));
                    show_toast().error(e);
                }
            }
            loading.set(false);
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

    rsx! {
        div { class: "mx-auto max-w-3xl px-4 py-6 md:px-6",
            div { class: "flex flex-wrap items-start justify-between gap-3",
                div { class: "min-w-0 flex-1",
                    h1 { class: "m-0 text-2xl font-semibold tracking-tight text-[var(--app-fg)]",
                        "Workers"
                    }
                    p { class: "mt-1 text-sm text-[var(--app-muted)]",
                        "Pair a machine; it phones home; the plane never dials inbound. Jobs run as processes on that machine (same as a self-hosted runner)."
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

                if hosts().is_empty() {
                    div {
                        class: "mt-6 rounded-xl border border-dashed border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-8 text-center",
                        p { class: "m-0 text-sm font-medium text-[var(--app-fg)]",
                            "No hosts yet"
                        }
                        p { class: "mt-2 text-sm text-[var(--app-muted)]",
                            "Add a worker to get a one-time pair token, then run the install commands on the machine."
                        }
                    }
                } else {
                    div { class: "mt-6 flex flex-col gap-3",
                        for host in hosts() {
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
            }
        }
    }
}

#[component]
fn PairPanel(created: CreateHostResponse, on_dismiss: EventHandler<MouseEvent>) -> Element {
    let commands = install_commands(&created);
    let expires = format_ts(&created.pair_expires_at);
    let token = created.pair_token.clone();
    let commands_copy = commands.clone();
    let loopback_hint = hint_is_loopback(&created.install_hint) || hint_is_loopback(&commands);

    rsx! {
        section {
            class: "mt-6 rounded-2xl border border-amber-500/30 bg-[var(--app-bg-elevated)] p-4",
            h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                "Pair this worker"
            }
            p { class: "mt-1 text-xs text-amber-300/90",
                "This pair token is shown once. Copy it now — it will not be shown again."
            }
            p { class: "mt-2 text-xs text-[var(--app-muted)]",
                "Expires {expires}"
            }
            if loopback_hint {
                p { class: "mt-2 text-xs text-amber-300/90",
                    "This install hint uses a loopback URL. Off-box machines need a reachable control-plane URL."
                }
            }

            label { class: "mt-3 block text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)]",
                "Pair token"
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
                        copy_text(&token);
                        show_toast().success("Token copied.");
                    },
                    "Copy"
                }
            }

            label { class: "mt-3 block text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)]",
                "Install"
            }
            div { class: "mt-1 flex flex-wrap items-start gap-2",
                pre {
                    class: "m-0 min-w-0 flex-1 overflow-x-auto whitespace-pre-wrap rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 font-mono text-xs text-[var(--app-fg)]",
                    "{commands}"
                }
                button {
                    class: "rounded-lg border border-[var(--app-border)] px-2.5 py-1.5 text-xs font-medium text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)]",
                    r#type: "button",
                    onclick: move |_| {
                        copy_text(&commands_copy);
                        show_toast().success("Install commands copied.");
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

#[component]
fn HostCard(
    workspace_id: String,
    host: HostPublic,
    on_updated: EventHandler<HostPublic>,
    on_revoked: EventHandler<String>,
) -> Element {
    let mut busy = use_signal(|| false);
    let mut confirm_revoke = use_signal(|| false);
    let revoked = host.status == "disabled";
    let pill = status_pill_class(&host.status);
    let last_seen = format_ts_opt(&host.last_seen_at);
    let can_host = host.allow_host && host.advertised_host;
    // v1 is host-only; advertised container is not a working executor.
    let no_effective = matches!(host.status.as_str(), "online" | "offline") && !can_host;

    let hostname = host.hostname.clone().unwrap_or_default();
    let platform = host.platform.clone().unwrap_or_default();
    let last_error = host.last_error.clone().unwrap_or_default();

    rsx! {
        article { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
            div { class: "flex flex-wrap items-start justify-between gap-2",
                div { class: "min-w-0 flex flex-wrap items-center gap-2",
                    h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                        "{host.name}"
                    }
                    span {
                        class: "inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-medium {pill}",
                        "{host.status}"
                    }
                }
                span { class: "text-xs text-[var(--app-muted)]",
                    "last seen {last_seen}"
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

            div { class: "mt-2 flex flex-wrap items-center gap-2 text-xs text-[var(--app-muted)]",
                span { "Advertised" }
                if host.advertised_host {
                    span { class: "rounded-full border border-[var(--app-border)] px-2 py-0.5 text-[var(--app-fg)]",
                        "host"
                    }
                }
                if host.advertised_container {
                    span { class: "rounded-full border border-[var(--app-border)] px-2 py-0.5 text-[var(--app-fg)]",
                        "container"
                    }
                }
                if !host.advertised_host && !host.advertised_container {
                    span { "none yet" }
                }
                span { class: "text-[var(--app-border)]", "·" }
                span { "max {host.max_concurrent}" }
            }

            if no_effective {
                p { class: "mt-2 text-xs text-amber-300/90",
                    "No effective run mode — this host will not receive jobs."
                }
            }
            if !last_error.is_empty() {
                p { class: "mt-2 text-xs text-red-400", "{last_error}" }
            }

            div { class: "mt-3 flex flex-wrap items-center gap-x-4 gap-y-2",
                label { class: "inline-flex items-center gap-1.5 text-xs text-[var(--app-fg)]",
                    input {
                        r#type: "checkbox",
                        checked: host.paused,
                        disabled: busy() || revoked,
                        onchange: {
                            let workspace_id = workspace_id.clone();
                            let host = host.clone();
                            move |_| {
                                if busy() || revoked {
                                    return;
                                }
                                busy.set(true);
                                let ws = workspace_id.clone();
                                let current = host.clone();
                                let patch = HostPatch {
                                    paused: Some(!host.paused),
                                    ..HostPatch::default()
                                };
                                spawn(async move {
                                    match patch_host(&ws, &current.id, &current, &patch).await {
                                        Ok(updated) => on_updated.call(updated),
                                        Err(e) => show_toast().error(e),
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                    }
                    "Paused"
                }
                label { class: "inline-flex items-center gap-1.5 text-xs text-[var(--app-fg)]",
                    input {
                        r#type: "checkbox",
                        checked: host.allow_host,
                        disabled: busy() || revoked,
                        onchange: {
                            let workspace_id = workspace_id.clone();
                            let host = host.clone();
                            move |_| {
                                if busy() || revoked {
                                    return;
                                }
                                busy.set(true);
                                let ws = workspace_id.clone();
                                let current = host.clone();
                                let patch = HostPatch {
                                    allow_host: Some(!host.allow_host),
                                    ..HostPatch::default()
                                };
                                spawn(async move {
                                    match patch_host(&ws, &current.id, &current, &patch).await {
                                        Ok(updated) => on_updated.call(updated),
                                        Err(e) => show_toast().error(e),
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                    }
                    "Allow host"
                }
                label { class: "inline-flex items-center gap-1.5 text-xs text-[var(--app-fg)]",
                    input {
                        r#type: "checkbox",
                        checked: host.allow_container,
                        disabled: true,
                        onchange: {
                            let workspace_id = workspace_id.clone();
                            let host = host.clone();
                            move |_| {
                                if busy() || revoked {
                                    return;
                                }
                                busy.set(true);
                                let ws = workspace_id.clone();
                                let current = host.clone();
                                let patch = HostPatch {
                                    allow_container: Some(!host.allow_container),
                                    ..HostPatch::default()
                                };
                                spawn(async move {
                                    match patch_host(&ws, &current.id, &current, &patch).await {
                                        Ok(updated) => on_updated.call(updated),
                                        Err(e) => show_toast().error(e),
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                    }
                    "Allow container"
                    span { class: "text-[var(--app-muted)]",
                        "(not shipped)"
                    }
                }
                label { class: "inline-flex items-center gap-1.5 text-xs text-[var(--app-fg)]",
                    "max"
                    input {
                        class: "{field_class()} w-16",
                        r#type: "number",
                        min: "1",
                        max: "256",
                        value: "{host.max_concurrent}",
                        disabled: busy() || revoked,
                        onchange: {
                            let workspace_id = workspace_id.clone();
                            let host = host.clone();
                            move |e| {
                                if busy() || revoked {
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
                                    match patch_host(&ws, &current.id, &current, &patch).await {
                                        Ok(updated) => on_updated.call(updated),
                                        Err(e) => show_toast().error(e),
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                    }
                }

                if !revoked {
                    if confirm_revoke() {
                        span { class: "inline-flex flex-wrap items-center gap-2 text-xs",
                            span { class: "text-[var(--app-muted)]",
                                "Revoke this host?"
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
                                                    show_toast().success("Worker revoked.");
                                                }
                                                Err(e) => show_toast().error(e),
                                            }
                                            busy.set(false);
                                            confirm_revoke.set(false);
                                        });
                                    }
                                },
                                "Confirm"
                            }
                            button {
                                class: "rounded-lg border border-[var(--app-border)] px-2.5 py-1 text-xs text-[var(--app-muted)] hover:text-[var(--app-fg)]",
                                r#type: "button",
                                disabled: busy(),
                                onclick: move |_| confirm_revoke.set(false),
                                "Cancel"
                            }
                        }
                    } else {
                        button {
                            class: "rounded-lg border border-red-500/30 px-2.5 py-1 text-xs font-semibold text-red-300 hover:bg-red-500/15 disabled:opacity-50",
                            r#type: "button",
                            disabled: busy(),
                            onclick: move |_| confirm_revoke.set(true),
                            "Revoke"
                        }
                    }
                }
            }
        }
    }
}
