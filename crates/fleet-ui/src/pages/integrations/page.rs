//! Integrations — card grid; OAuth opens in a new tab with button spinner.

use dioxus::prelude::*;

use super::api::{
    Integration, IntegrationCategory, connect_gogs, integration_is_connected, list_integrations,
    live_connections, load_integrations, oauth_start_url_setup, patch_integration_defaults,
};
use super::logos::IntegrationLogo;
use crate::Route;
use crate::components::toast::show_toast;
use crate::pages::inbox::is_inbox_app;

/// How long we poll for a completed OAuth before giving up (~3 min).
const OAUTH_POLL_ATTEMPTS: u32 = 90;
const OAUTH_POLL_MS: u32 = 1500;

/// Result from the auth tab (`postMessage`). Written **only** from JS listeners
/// (outside the Dioxus runtime). Read from async tasks started with `spawn`.
#[derive(Clone, Debug)]
struct OauthBridgeMsg {
    ok: bool,
    provider: String,
    error: String,
}

#[cfg(target_arch = "wasm32")]
mod oauth_bridge {
    use super::OauthBridgeMsg;
    use std::cell::RefCell;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::prelude::*;

    thread_local! {
        static PENDING: RefCell<Option<OauthBridgeMsg>> = const { RefCell::new(None) };
        static LISTENER_INSTALLED: RefCell<bool> = const { RefCell::new(false) };
    }

    pub fn take() -> Option<OauthBridgeMsg> {
        // Prefer postMessage; also drain localStorage fallback from /oauth/done.
        if let Some(msg) = PENDING.with(|p| p.borrow_mut().take()) {
            return Some(msg);
        }
        take_from_local_storage()
    }

    fn take_from_local_storage() -> Option<OauthBridgeMsg> {
        let window = web_sys::window()?;
        let storage = window.local_storage().ok().flatten()?;
        let raw = storage.get_item("fleet_oauth_result").ok().flatten()?;
        let _ = storage.remove_item("fleet_oauth_result");
        if let Some(rest) = raw.strip_prefix("connected:") {
            return Some(OauthBridgeMsg {
                ok: true,
                provider: rest.to_string(),
                error: String::new(),
            });
        }
        if let Some(rest) = raw.strip_prefix("error:") {
            return Some(OauthBridgeMsg {
                ok: false,
                provider: String::new(),
                error: rest.to_string(),
            });
        }
        None
    }

    fn push(msg: OauthBridgeMsg) {
        PENDING.with(|p| {
            *p.borrow_mut() = Some(msg);
        });
    }

    /// Install window `message` + storage listeners once. Never calls Dioxus APIs.
    pub fn ensure_listener() {
        LISTENER_INSTALLED.with(|flag| {
            if *flag.borrow() {
                return;
            }
            *flag.borrow_mut() = true;

            let handler = Closure::wrap(Box::new(move |event: web_sys::MessageEvent| {
                let data = event.data();
                let source = js_sys::Reflect::get(&data, &JsValue::from_str("source"))
                    .ok()
                    .and_then(|v| v.as_string())
                    .unwrap_or_default();
                if source != "fleet-oauth" {
                    return;
                }
                let ok = js_sys::Reflect::get(&data, &JsValue::from_str("ok"))
                    .ok()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let provider = js_sys::Reflect::get(&data, &JsValue::from_str("provider"))
                    .ok()
                    .and_then(|v| v.as_string())
                    .unwrap_or_default();
                let error = js_sys::Reflect::get(&data, &JsValue::from_str("error"))
                    .ok()
                    .and_then(|v| v.as_string())
                    .unwrap_or_default();

                push(OauthBridgeMsg {
                    ok,
                    provider,
                    error,
                });
            }) as Box<dyn FnMut(_)>);

            // Cross-tab localStorage writes (oauth_done page) fire StorageEvent here.
            let storage_handler = Closure::wrap(Box::new(move |event: web_sys::StorageEvent| {
                let Some(key) = event.key() else {
                    return;
                };
                if key != "fleet_oauth_result" {
                    return;
                }
                let Some(raw) = event.new_value() else {
                    return;
                };
                if let Some(rest) = raw.strip_prefix("connected:") {
                    push(OauthBridgeMsg {
                        ok: true,
                        provider: rest.to_string(),
                        error: String::new(),
                    });
                } else if let Some(rest) = raw.strip_prefix("error:") {
                    push(OauthBridgeMsg {
                        ok: false,
                        provider: String::new(),
                        error: rest.to_string(),
                    });
                }
            }) as Box<dyn FnMut(_)>);

            if let Some(window) = web_sys::window() {
                let _ = window
                    .add_event_listener_with_callback("message", handler.as_ref().unchecked_ref());
                let _ = window.add_event_listener_with_callback(
                    "storage",
                    storage_handler.as_ref().unchecked_ref(),
                );
                // Leak listeners for the lifetime of the page (SPA).
                handler.forget();
                storage_handler.forget();
            }
        });
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod oauth_bridge {
    use super::OauthBridgeMsg;
    pub fn take() -> Option<OauthBridgeMsg> {
        None
    }
    pub fn ensure_listener() {}
}

#[component]
pub fn Integrations(workspace_id: String) -> Element {
    let mut items = use_signal(|| Vec::<Integration>::new());
    let mut categories = use_signal(|| Vec::<IntegrationCategory>::new());
    let mut loading = use_signal(|| true);
    let mut error = use_signal(|| Option::<String>::None);
    // Provider currently connecting (OAuth tab open) — shows spinner on Add.
    let mut connecting_provider = use_signal(|| Option::<String>::None);
    let mut query = use_signal(|| String::new());

    // Safe: only installs a JS listener that writes to thread_local (no signals).
    use_effect(move || {
        oauth_bridge::ensure_listener();
    });

    let workspace_id_load = workspace_id.clone();
    use_effect(move || {
        let ws = workspace_id_load.clone();
        spawn(async move {
            loading.set(true);
            match load_integrations(&ws).await {
                Ok(snapshot) => {
                    items.set(snapshot.integrations);
                    categories.set(snapshot.categories);
                    error.set(None);
                }
                Err(e) => {
                    let msg = e.message();
                    error.set(Some(msg.clone()));
                    show_toast().error(msg);
                }
            }
            loading.set(false);
        });
    });

    let q = query().trim().to_lowercase();
    let filtered: Vec<Integration> = items()
        .into_iter()
        .filter(|i| {
            if q.is_empty() {
                return true;
            }
            i.name.to_lowercase().contains(&q)
                || i.description.to_lowercase().contains(&q)
                || i.category_label.to_lowercase().contains(&q)
                || i.id.to_lowercase().contains(&q)
        })
        .collect();

    rsx! {
        div { class: "flex h-full w-full flex-col overflow-auto p-4 md:p-5",
            div { class: "mb-5",
                h1 { class: "m-0 text-2xl font-semibold tracking-tight text-[var(--app-fg)]",
                    "Integrations"
                }
                p { class: "mt-1 text-sm text-[var(--app-muted)]",
                    "Connected accounts are listed first. Add another account for the same service with its own name. Tokens stay on the server."
                }
            }

            div { class: "relative mb-4",
                div {
                    class: "pointer-events-none absolute inset-y-0 left-0 flex items-center pl-3.5 text-[var(--app-muted)]",
                    svg {
                        class: "h-4 w-4",
                        view_box: "0 0 24 24",
                        fill: "none",
                        stroke: "currentColor",
                        stroke_width: "2",
                        stroke_linecap: "round",
                        stroke_linejoin: "round",
                        circle { cx: "11", cy: "11", r: "7" }
                        path { d: "M21 21l-4.3-4.3" }
                    }
                }
                input {
                    class: "w-full rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] py-2.5 pl-10 pr-3 text-sm text-[var(--app-fg)] outline-none placeholder:text-[var(--app-muted)] focus:border-neutral-500",
                    r#type: "search",
                    placeholder: "Search integrations…",
                    value: "{query}",
                    oninput: move |e| query.set(e.value()),
                }
            }

            if loading() {
                div {
                    class: "grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3",
                    for i in 0..6 {
                        IntegrationSkeletonCard { key: "{i}" }
                    }
                }
            } else if let Some(err) = error() {
                div {
                    class: "rounded-xl border border-red-500/25 bg-red-500/10 px-4 py-3 text-sm text-red-300",
                    "{err}"
                }
            } else if filtered.is_empty() {
                p { class: "text-sm text-[var(--app-muted)]", "No integrations match your search." }
            } else {
                IntegrationSections {
                    workspace_id: workspace_id.clone(),
                    integrations: filtered,
                    categories: categories(),
                    connecting_provider,
                    items,
                }
            }

        }
    }
}

#[component]
fn IntegrationSections(
    workspace_id: String,
    integrations: Vec<Integration>,
    categories: Vec<IntegrationCategory>,
    connecting_provider: Signal<Option<String>>,
    mut items: Signal<Vec<Integration>>,
) -> Element {
    let groups = if categories.is_empty() {
        let mut seen = Vec::<IntegrationCategory>::new();
        for item in &integrations {
            if seen.iter().any(|c| c.id == item.category_id) {
                continue;
            }
            seen.push(IntegrationCategory {
                id: item.category_id.clone(),
                label: item.category_label.clone(),
                subtitle: String::new(),
                sort_order: seen.len() as i32,
            });
        }
        seen
    } else {
        categories.clone()
    };
    let connected_count: usize = integrations.iter().map(|i| live_connections(i).len()).sum();

    rsx! {
        section { class: "mb-8",
            h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Connected" }
            p { class: "mt-1 mb-3 text-xs text-[var(--app-muted)]",
                if connected_count == 0 {
                    "No accounts connected yet. Add one below."
                } else {
                    "Each name is a separate account. Manage opens that account so you can rename it, update sign-in, or remove it."
                }
            }
            for group in groups.clone() {
                {
                    let rows: Vec<(Integration, super::api::ConnectionPublic)> = integrations
                        .iter()
                        .filter(|i| i.category_id == group.id)
                        .flat_map(|i| {
                            live_connections(i)
                                .into_iter()
                                .map(|c| (i.clone(), c))
                                .collect::<Vec<_>>()
                        })
                        .collect();
                    if rows.is_empty() {
                        rsx! { div { key: "c-{group.id}" } }
                    } else {
                        rsx! {
                            div { key: "c-{group.id}", class: "mb-4",
                                h3 { class: "m-0 mb-2 text-xs font-semibold uppercase tracking-wide text-[var(--app-muted)]",
                                    "{group.label}"
                                }
                                div { class: "grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3",
                                    for (integ, conn) in rows {
                                        ConnectedCard {
                                            key: "{conn.id}",
                                            workspace_id: workspace_id.clone(),
                                            integ: integ.clone(),
                                            connection_id: conn.id.clone(),
                                            display_name: conn.display_name.clone(),
                                            account_label: conn.account_label.clone().unwrap_or_default(),
                                            last_error: conn.last_error.clone().unwrap_or_default(),
                                            failed: conn.status.eq_ignore_ascii_case("error"),
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        section {
            h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Add an integration" }
            p { class: "mt-1 mb-3 text-xs text-[var(--app-muted)]",
                "Every service Fleet can connect. Add asks for a name, then sign-in. The same service can be added again under another name."
            }
            for group in groups {
                {
                    let rows: Vec<Integration> = integrations
                        .iter()
                        .filter(|i| i.category_id == group.id)
                        .cloned()
                        .collect();
                    if rows.is_empty() {
                        rsx! { div { key: "a-{group.id}" } }
                    } else {
                        rsx! {
                            div { key: "a-{group.id}", class: "mb-4",
                                h3 { class: "m-0 mb-1 text-xs font-semibold uppercase tracking-wide text-[var(--app-muted)]",
                                    "{group.label}"
                                }
                                if !group.subtitle.is_empty() {
                                    p { class: "m-0 mb-2 text-xs text-[var(--app-muted)]", "{group.subtitle}" }
                                }
                                div { class: "grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3",
                                    for integ in rows {
                                        AddCard {
                                            key: "add-{integ.id}",
                                            workspace_id: workspace_id.clone(),
                                            integ: integ.clone(),
                                            connecting_provider,
                                            items,
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

#[component]
fn ConnectedCard(
    workspace_id: String,
    integ: Integration,
    connection_id: String,
    display_name: String,
    account_label: String,
    last_error: String,
    failed: bool,
) -> Element {
    let title = if display_name.trim().is_empty() {
        integ.name.clone()
    } else {
        display_name.clone()
    };
    let border = if failed {
        "border-red-500/60"
    } else {
        "border-[var(--app-border)]"
    };
    rsx! {
        article { class: "flex flex-col rounded-2xl border {border} bg-[var(--app-bg-elevated)] p-4",
            div { class: "flex items-start gap-3",
                IntegrationLogo { icon: integ.icon.clone(), name: integ.name.clone() }
                div { class: "min-w-0 flex-1",
                    h2 { class: "m-0 truncate text-[15px] font-semibold text-[var(--app-fg)]", "{title}" }
                    p { class: "m-0 mt-0.5 text-xs text-[var(--app-muted)]",
                        "{integ.name}"
                        if !account_label.is_empty() { " · {account_label}" }
                    }
                    if failed {
                        p { class: "m-0 mt-1 text-xs font-semibold text-red-300",
                            "Sign-in failed"
                            if !last_error.is_empty() { " · {last_error}" }
                        }
                    } else {
                        p { class: "m-0 mt-1 text-xs text-[var(--app-muted)]", "Connected" }
                    }
                }
            }
            div { class: "mt-3 flex flex-wrap gap-2",
                Link {
                    to: Route::ConnectionEdit {
                        workspace_id: workspace_id.clone(),
                        provider: integ.id.clone(),
                        connection_id: connection_id.clone(),
                    },
                    class: "inline-flex items-center text-xs font-semibold text-[var(--app-accent)] underline",
                    "Manage"
                }
                if is_inbox_app(&integ.id) {
                    Link {
                        to: Route::InboxConnection {
                            workspace_id: workspace_id.clone(),
                            provider: integ.id.clone(),
                            connection_id: connection_id.clone(),
                        },
                        class: "inline-flex items-center text-xs text-[var(--app-accent)] underline",
                        "Open inbox"
                    }
                }
            }
        }
    }
}

#[component]
fn AddCard(
    workspace_id: String,
    integ: Integration,
    mut connecting_provider: Signal<Option<String>>,
    mut items: Signal<Vec<Integration>>,
) -> Element {
    let available = integ.availability == "available";
    let mut open = use_signal(|| false);
    let mut draft_name = use_signal(String::new);
    let mut draft_url = use_signal(String::new);
    let mut draft_user = use_signal(String::new);
    let mut draft_pass = use_signal(String::new);
    let connecting = connecting_provider().as_deref() == Some(integ.id.as_str());
    let ask_url = super::connection::needs_instance_url(&integ.id);
    let ask_gogs = integ.id == "gogs";
    let provider = integ.id.clone();
    let provider_name = integ.name.clone();
    rsx! {
        article { class: "flex min-h-[7.5rem] flex-col rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
            div { class: "flex items-start gap-3",
                IntegrationLogo { icon: integ.icon.clone(), name: integ.name.clone() }
                div { class: "min-w-0",
                    h2 { class: "m-0 truncate text-[15px] font-semibold text-[var(--app-fg)]", "{integ.name}" }
                    p { class: "m-0 mt-0.5 text-xs text-[var(--app-muted)]", "{integ.category_label}" }
                }
            }
            p { class: "mt-3 line-clamp-2 flex-1 text-[13px] leading-snug text-[var(--app-muted)]",
                "{integ.description}"
            }
            if available {
                button {
                    class: "mt-3 inline-flex items-center justify-center rounded-lg border border-[var(--app-border)] px-2.5 py-1 text-xs font-semibold text-[var(--app-fg)] disabled:opacity-60",
                    r#type: "button",
                    disabled: connecting,
                    onclick: move |_| open.set(true),
                    if connecting { "Connecting" } else { "Add" }
                }
                if open() {
                    div { class: "fixed inset-0 z-50 flex items-center justify-center bg-black/50 p-4",
                        div { class: "w-full max-w-md rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-5",
                            h3 { class: "m-0 text-base font-semibold text-[var(--app-fg)]", "Add {provider_name}" }
                            p { class: "mt-1 text-sm text-[var(--app-muted)]",
                                "Name this account. You can add the same service again under a different name."
                            }
                            label { class: "mt-4 block text-xs text-[var(--app-muted)]",
                                "Name"
                                input {
                                    class: "mt-1 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                                    placeholder: "Work, Personal, …",
                                    value: "{draft_name}",
                                    oninput: move |e| draft_name.set(e.value()),
                                }
                            }
                            if ask_url || ask_gogs {
                                label { class: "mt-3 block text-xs text-[var(--app-muted)]",
                                    if ask_gogs { "Gogs URL" } else { "Gitea URL" }
                                    input {
                                        class: "mt-1 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                                        placeholder: if ask_gogs { "http://localhost:3034" } else { "https://gitea.example.com" },
                                        value: "{draft_url}",
                                        oninput: move |e| draft_url.set(e.value()),
                                    }
                                }
                            }
                            if ask_gogs {
                                label { class: "mt-3 block text-xs text-[var(--app-muted)]",
                                    "Username"
                                    input {
                                        class: "mt-1 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                                        value: "{draft_user}",
                                        oninput: move |e| draft_user.set(e.value()),
                                    }
                                }
                                label { class: "mt-3 block text-xs text-[var(--app-muted)]",
                                    "Password"
                                    input {
                                        class: "mt-1 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                                        r#type: "password",
                                        value: "{draft_pass}",
                                        oninput: move |e| draft_pass.set(e.value()),
                                    }
                                }
                            }
                            div { class: "mt-4 flex justify-end gap-2",
                                button {
                                    class: "rounded-lg px-3 py-2 text-sm text-[var(--app-muted)]",
                                    r#type: "button",
                                    onclick: move |_| open.set(false),
                                    "Cancel"
                                }
                                button {
                                    class: "rounded-lg bg-[var(--app-accent)] px-3 py-2 text-sm font-semibold text-[var(--app-accent-fg)]",
                                    r#type: "button",
                                    onclick: {
                                        let ws = workspace_id.clone();
                                        let provider = provider.clone();
                                        let label = provider_name.clone();
                                        move |_| {
                                            let name = draft_name().trim().to_string();
                                            if name.is_empty() {
                                                show_toast().error("Name this connection.");
                                                return;
                                            }
                                            if (ask_url || ask_gogs) && draft_url().trim().is_empty() {
                                                show_toast().error(if ask_gogs {
                                                    "Enter the Gogs URL for this connection."
                                                } else {
                                                    "Enter the Gitea URL for this connection."
                                                });
                                                return;
                                            }
                                            if ask_gogs && (draft_user().trim().is_empty() || draft_pass().is_empty()) {
                                                show_toast().error("Enter the Gogs username and password.");
                                                return;
                                            }
                                            open.set(false);
                                            if ask_gogs {
                                                let base_url = draft_url().trim().to_string();
                                                let username = draft_user().trim().to_string();
                                                let password = draft_pass();
                                                let ws = ws.clone();
                                                let provider = provider.clone();
                                                connecting_provider.set(Some(provider.clone()));
                                                spawn(async move {
                                                    match connect_gogs(&ws, &base_url, &username, &password, &name).await {
                                                        Ok(()) => {
                                                            poll_until_connected(&ws, &provider, &name, connecting_provider, items).await;
                                                        }
                                                        Err(err) => {
                                                            connecting_provider.set(None);
                                                            show_toast().error(err.message());
                                                        }
                                                    }
                                                });
                                                return;
                                            }
                                            let url = oauth_start_url_setup(&ws, &provider, &name, draft_url().trim());
                                            #[cfg(target_arch = "wasm32")]
                                            {
                                                if !open_oauth_tab(&url) {
                                                    show_toast().error("Could not open auth tab. Allow popups for this site, then try again.");
                                                    return;
                                                }
                                            }
                                            #[cfg(not(target_arch = "wasm32"))]
                                            {
                                                let _ = url;
                                            }
                                            connecting_provider.set(Some(provider.clone()));
                                            show_toast().info(format!("Authorize {label} in the new tab. This page will update when done."));
                                            let ws = ws.clone();
                                            let provider = provider.clone();
                                            spawn(async move {
                                                poll_until_connected(&ws, &provider, &name, connecting_provider, items).await;
                                            });
                                        }
                                    },
                                    "Continue"
                                }
                            }
                        }
                    }
                }
            } else {
                p { class: "mt-3 text-xs text-[var(--app-muted)]", "Coming soon" }
            }
        }
    }
}

#[component]
fn IntegrationDefaults(workspace_id: String, integ: Integration) -> Element {
    use crate::pages::runners::self_hosted::api::{HostPublic, list_workers_page};
    use fleet_runners_view::{display_name, is_removed};

    let mut hosts = use_signal(Vec::<HostPublic>::new);
    let defaults = integ
        .connection
        .as_ref()
        .map(|c| c.defaults.clone())
        .unwrap_or_default();
    let mut runner = use_signal(|| defaults.default_runner_id.clone().unwrap_or_default());
    let mut provider = use_signal(|| defaults.default_provider.clone().unwrap_or_default());
    let mut model = use_signal(|| defaults.default_model.clone().unwrap_or_default());
    let ws = workspace_id.clone();
    use_effect(move || {
        let ws = ws.clone();
        spawn(async move {
            if let Ok((list, _)) = list_workers_page(&ws).await {
                hosts.set(list.into_iter().filter(|h| !is_removed(h)).collect());
            }
        });
    });
    let selected = hosts().into_iter().find(|h| h.id == runner());
    let agents = selected
        .as_ref()
        .map(|h| {
            h.agents
                .iter()
                .filter(|a| a.installed)
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let models = agents
        .iter()
        .find(|a| a.id == provider())
        .map(|a| a.models.clone())
        .unwrap_or_default();
    rsx! {
        div { class: "mt-3 space-y-1.5 border-t border-[var(--app-border)] pt-3",
            p { class: "m-0 text-[10px] uppercase tracking-wide text-[var(--app-muted)]",
                "Default runner / provider / model"
            }
            p { class: "m-0 text-[11px] text-[var(--app-muted)]",
                "Used when Inbox or Chat launches work from this app."
            }
            div { class: "flex flex-wrap gap-1.5",
                select {
                    class: "max-w-[8rem] rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-1.5 py-1 text-[11px] text-[var(--app-fg)]",
                    value: "{runner}",
                    onchange: move |e| runner.set(e.value()),
                    option { value: "", "Runner" }
                    for h in hosts() {
                        option { value: "{h.id}", selected: runner() == h.id, "{display_name(&h)}" }
                    }
                }
                select {
                    class: "max-w-[7rem] rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-1.5 py-1 text-[11px] text-[var(--app-fg)]",
                    value: "{provider}",
                    onchange: move |e| provider.set(e.value()),
                    option { value: "", "Provider" }
                    for a in agents {
                        option {
                            value: "{a.id}",
                            selected: provider() == a.id,
                            if a.logged_in { "{a.id}" } else { "{a.id} · logged out" }
                        }
                    }
                }
                select {
                    class: "max-w-[8rem] rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-1.5 py-1 text-[11px] text-[var(--app-fg)]",
                    value: "{model}",
                    onchange: move |e| model.set(e.value()),
                    option { value: "", "Model" }
                    for m in models {
                        option { value: "{m}", selected: model() == m, "{m}" }
                    }
                }
                button {
                    class: "rounded-lg border border-[var(--app-border)] px-2 py-1 text-[11px] font-medium text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)]",
                    r#type: "button",
                    onclick: {
                        let ws = workspace_id.clone();
                        let pid = integ.id.clone();
                        move |_| {
                            let ws = ws.clone();
                            let pid = pid.clone();
                            let r = runner();
                            let p = provider();
                            let m = model();
                            spawn(async move {
                                match patch_integration_defaults(
                                    &ws,
                                    &pid,
                                    if r.is_empty() { None } else { Some(r.as_str()) },
                                    if p.is_empty() { None } else { Some(p.as_str()) },
                                    if m.is_empty() { None } else { Some(m.as_str()) },
                                )
                                .await
                                {
                                    Ok(()) => show_toast().success("Defaults saved."),
                                    Err(e) => show_toast().error(e.message()),
                                }
                            });
                        }
                    },
                    "Save"
                }
            }
        }
    }
}

/// Placeholder card that mirrors the real integration card geometry while loading.
#[component]
fn IntegrationSkeletonCard() -> Element {
    rsx! {
        article {
            class: "flex min-h-[7.5rem] flex-col rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
            div { class: "flex items-start gap-3",
                div {
                    class: "h-10 w-10 shrink-0 animate-pulse rounded-xl bg-[var(--app-bg-hover)]"
                }
                div { class: "min-w-0 flex-1 space-y-2 pt-0.5",
                    div { class: "h-3.5 w-24 animate-pulse rounded bg-[var(--app-bg-hover)]" }
                    div { class: "h-2.5 w-16 animate-pulse rounded bg-[var(--app-bg-hover)]" }
                }
                div { class: "h-7 w-[5.5rem] shrink-0 animate-pulse rounded-lg bg-[var(--app-bg-hover)]" }
            }
            div { class: "mt-3 space-y-1.5",
                div { class: "h-2.5 w-full animate-pulse rounded bg-[var(--app-bg-hover)]" }
                div { class: "h-2.5 w-4/5 max-w-[85%] animate-pulse rounded bg-[var(--app-bg-hover)]" }
            }
        }
    }
}

#[component]
fn Spinner() -> Element {
    rsx! {
        svg {
            class: "h-3.5 w-3.5 animate-spin",
            view_box: "0 0 24 24",
            fill: "none",
            "aria-hidden": "true",
            circle {
                class: "opacity-25",
                cx: "12",
                cy: "12",
                r: "10",
                stroke: "currentColor",
                stroke_width: "4",
            }
            path {
                class: "opacity-90",
                fill: "currentColor",
                d: "M4 12a8 8 0 018-8v3a5 5 0 00-5 5H4z",
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn open_oauth_tab(url: &str) -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    // Full new tab (no popup chrome). Named target keeps window.opener so the
    // auth tab can close itself after OAuth (Chrome's _blank defaults to noopener).
    match window.open_with_url_and_target(url, "fleet_oauth") {
        Ok(Some(tab)) => {
            let _ = tab.focus();
            true
        }
        Ok(None) => false,
        Err(_) => false,
    }
}

/// Optimistically flip one provider to connected so the card updates immediately.
fn mark_connected_locally(mut items: Signal<Vec<Integration>>, provider: &str) {
    let mut list = items();
    let mut changed = false;
    for integ in list.iter_mut() {
        if integ.id.eq_ignore_ascii_case(provider) {
            integ.connected = true;
            changed = true;
            break;
        }
    }
    if changed {
        items.set(list);
    }
}

fn named_connection_is_live(list: &[Integration], provider: &str, name: &str) -> bool {
    list.iter().any(|i| {
        if !i.id.eq_ignore_ascii_case(provider) {
            return false;
        }
        if name.is_empty() {
            return integration_is_connected(i);
        }
        live_connections(i).iter().any(|c| {
            c.display_name.eq_ignore_ascii_case(name)
                && !c.status.eq_ignore_ascii_case("error")
                && (c.connected || c.status.eq_ignore_ascii_case("connected"))
        })
    })
}

async fn refresh_until_connected(
    workspace_id: &str,
    provider: &str,
    name: &str,
    mut items: Signal<Vec<Integration>>,
    attempts: u32,
    delay_ms: u32,
) -> bool {
    for _ in 0..attempts {
        match list_integrations(workspace_id).await {
            Ok(list) => {
                let done = named_connection_is_live(&list, provider, name);
                items.set(list);
                if done {
                    return true;
                }
            }
            Err(_) => {}
        }
        sleep_ms(delay_ms).await;
    }
    false
}

async fn poll_until_connected(
    workspace_id: &str,
    provider: &str,
    name: &str,
    mut connecting_provider: Signal<Option<String>>,
    mut items: Signal<Vec<Integration>>,
) {
    for _ in 0..OAUTH_POLL_ATTEMPTS {
        if connecting_provider().as_deref() != Some(provider) {
            return;
        }

        // Auth tab postMessage / localStorage → thread_local (safe).
        if let Some(msg) = oauth_bridge::take() {
            if msg.ok {
                let matches =
                    msg.provider.is_empty() || msg.provider.eq_ignore_ascii_case(provider);
                if matches {
                    // Instant UI feedback — don't wait for the next list fetch.
                    mark_connected_locally(items, provider);

                    // Confirm with server (DB may lag a beat behind the popup).
                    let confirmed =
                        refresh_until_connected(workspace_id, provider, name, items, 20, 300).await;

                    connecting_provider.set(None);
                    if confirmed {
                        show_toast().success(format!("{provider} connected."));
                    } else {
                        // Keep optimistic connected state; list will catch up on next visit.
                        show_toast().success(format!("{provider} connected."));
                    }
                    return;
                }
                // Message for a different provider — ignore.
            } else {
                connecting_provider.set(None);
                let msg_text = if msg.error.is_empty() {
                    "authorization failed".into()
                } else {
                    msg.error
                };
                show_toast().error(format!("Could not connect: {msg_text}"));
                return;
            }
        }

        // Background poll even without postMessage (popup closed / opener lost).
        if let Ok(list) = list_integrations(workspace_id).await {
            let done = named_connection_is_live(&list, provider, name);
            items.set(list);
            if done {
                connecting_provider.set(None);
                show_toast().success(format!("{provider} connected."));
                return;
            }
        }

        sleep_ms(OAUTH_POLL_MS).await;
    }

    if connecting_provider().as_deref() == Some(provider) {
        connecting_provider.set(None);
        show_toast().error(format!(
            "Still waiting for {provider}. Finish authorizing in the other tab, or try Connect again."
        ));
    }
}

async fn sleep_ms(ms: u32) {
    #[cfg(target_arch = "wasm32")]
    {
        gloo_timers::future::TimeoutFuture::new(ms).await;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = ms;
    }
}
