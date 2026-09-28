//! Integrations — card grid; OAuth opens in a new tab with button spinner.

use dioxus::prelude::*;

use super::api::{
    disconnect_integration, integration_is_connected, list_integrations, oauth_start_url,
    patch_integration_defaults, Integration,
};
use super::logos::IntegrationLogo;
use crate::components::toast::show_toast;
use crate::pages::inbox::is_inbox_app;
use crate::Route;

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
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;

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
    let mut loading = use_signal(|| true);
    let mut error = use_signal(|| Option::<String>::None);
    // Provider currently connecting (OAuth tab open) — shows spinner on Connect.
    let mut connecting_provider = use_signal(|| Option::<String>::None);
    let mut busy_provider = use_signal(|| Option::<String>::None);
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
            match list_integrations(&ws).await {
                Ok(list) => {
                    items.set(list);
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
                    "Connect tools with OAuth. Tokens stay on the server, never in sandboxes."
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
                p { class: "text-sm text-[var(--app-muted)]",
                    if q.is_empty() {
                        "No integrations available."
                    } else {
                        "No integrations match your search."
                    }
                }
            } else {
                div {
                    class: "grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3",
                    for integ in filtered {
                        {
                            let integ_disconnect = integ.clone();
                            let ws_disconnect = workspace_id.clone();
                            let ws_connect = workspace_id.clone();
                            let available = integ.availability == "available";
                            let connected = integration_is_connected(&integ);
                            let is_connecting =
                                connecting_provider().as_deref() == Some(integ.id.as_str());
                            let is_busy = busy_provider().as_deref() == Some(integ.id.as_str());
                            let account = integ
                                .connection
                                .as_ref()
                                .and_then(|c| c.account_label.clone())
                                .unwrap_or_default();
                            let repos = integ
                                .connection
                                .as_ref()
                                .map(crate::pages::integrations::api::granted_repos)
                                .unwrap_or_default();
                            let last_error = integ
                                .connection
                                .as_ref()
                                .and_then(|c| c.last_error.clone())
                                .filter(|s| !s.trim().is_empty());
                            let reconnect = !connected && last_error.is_some();

                            let subtitle = if connected && !account.is_empty() {
                                if repos.is_empty() {
                                    account.clone()
                                } else {
                                    format!("{account} · {}", repos.join(", "))
                                }
                            } else if is_connecting {
                                "Connecting…".to_string()
                            } else if !available {
                                "Coming soon".to_string()
                            } else {
                                integ.category_label.clone()
                            };

                            let status_dot = if connected {
                                "bg-emerald-400"
                            } else if is_connecting {
                                "bg-amber-400 animate-pulse"
                            } else if available {
                                "bg-neutral-500"
                            } else {
                                "bg-neutral-600"
                            };

                            let provider_connect = integ.id.clone();
                            let provider_name = integ.name.clone();

                            rsx! {
                                article {
                                    key: "{integ.id}",
                                    class: "flex min-h-[7.5rem] flex-col rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4 transition hover:border-neutral-600/90 hover:bg-[var(--app-bg-hover)]",

                                    div { class: "flex items-start gap-3",
                                        IntegrationLogo {
                                            icon: integ.icon.clone(),
                                            name: integ.name.clone(),
                                        }
                                        div { class: "min-w-0 flex-1",
                                            h2 { class: "m-0 truncate text-[15px] font-semibold tracking-tight text-[var(--app-fg)]",
                                                "{integ.name}"
                                            }
                                            div { class: "mt-0.5 flex flex-wrap items-center gap-x-1.5 gap-y-0.5 text-xs text-[var(--app-muted)]",
                                                span {
                                                    class: "inline-block h-1.5 w-1.5 shrink-0 rounded-full {status_dot}"
                                                }
                                                span { class: "min-w-0 break-all", title: "{subtitle}", "{subtitle}" }
                                                if connected {
                                                    span { class: "text-emerald-400/90", "· Connected" }
                                                } else if reconnect {
                                                    span { class: "text-red-300/90", "· Error" }
                                                }
                                            }
                                        }

                                        if available && !connected {
                                            button {
                                                class: "inline-flex min-w-[5.5rem] shrink-0 items-center justify-center gap-1.5 rounded-lg border border-[var(--app-border)] px-2.5 py-1 text-xs font-semibold text-[var(--app-fg)] transition hover:bg-white/5 disabled:cursor-wait disabled:opacity-80",
                                                r#type: "button",
                                                disabled: is_connecting,
                                                title: if is_connecting {
                                                    "Waiting for authorization in the other tab…"
                                                } else if reconnect {
                                                    "Check the existing app install and reconnect"
                                                } else {
                                                    "Sign in with the provider and grant permissions"
                                                },
                                                onclick: move |_| {
                                                    if connecting_provider().as_deref() == Some(provider_connect.as_str()) {
                                                        return;
                                                    }
                                                    let provider = provider_connect.clone();
                                                    let ws = ws_connect.clone();
                                                    let name = provider_name.clone();
                                                    let url = oauth_start_url(&ws, &provider);

                                                    #[cfg(target_arch = "wasm32")]
                                                    {
                                                        let opened = open_oauth_tab(&url);
                                                        if !opened {
                                                            show_toast().error(
                                                                "Could not open auth tab. Allow popups for this site, then try again.",
                                                            );
                                                            return;
                                                        }
                                                    }
                                                    #[cfg(not(target_arch = "wasm32"))]
                                                    {
                                                        let _ = url;
                                                    }

                                                    connecting_provider.set(Some(provider.clone()));
                                                    show_toast().info(format!(
                                                        "Authorize {name} in the new tab. This page will update when done."
                                                    ));

                                                    spawn(async move {
                                                        poll_until_connected(
                                                            &ws,
                                                            &provider,
                                                            connecting_provider,
                                                            items,
                                                        )
                                                        .await;
                                                    });
                                                },
                                                if is_connecting {
                                                    Spinner {}
                                                    span { "Connecting" }
                                                } else if reconnect {
                                                    span { "Reconnect" }
                                                } else {
                                                    span { "Connect" }
                                                }
                                            }
                                        } else if available && connected {
                                            button {
                                                class: "shrink-0 rounded-lg border border-red-500/30 px-2.5 py-1 text-xs font-semibold text-red-300 transition hover:bg-red-500/15 disabled:opacity-50",
                                                r#type: "button",
                                                disabled: is_busy,
                                                title: "Disconnect {integ.name}",
                                                onclick: move |_| {
                                                    let provider = integ_disconnect.id.clone();
                                                    let ws = ws_disconnect.clone();
                                                    busy_provider.set(Some(provider.clone()));
                                                    spawn(async move {
                                                        match disconnect_integration(&ws, &provider).await {
                                                            Ok(()) => {
                                                                if let Ok(list) = list_integrations(&ws).await {
                                                                    items.set(list);
                                                                }
                                                                show_toast().success(format!(
                                                                    "{provider} disconnected."
                                                                ));
                                                            }
                                                            Err(e) => {
                                                                show_toast().error(e.message());
                                                            }
                                                        }
                                                        busy_provider.set(None);
                                                    });
                                                },
                                                if is_busy {
                                                    Spinner {}
                                                } else {
                                                    span { "Disconnect" }
                                                }
                                            }
                                        } else {
                                            button {
                                                class: "shrink-0 cursor-not-allowed rounded-lg border border-[var(--app-border)] px-2.5 py-1 text-xs font-medium text-[var(--app-muted)] opacity-50",
                                                r#type: "button",
                                                disabled: true,
                                                if !available { "Soon" } else { "Setup" }
                                            }
                                        }
                                    }

                                    p { class: "mt-3 line-clamp-2 flex-1 text-[13px] leading-snug text-[var(--app-muted)]",
                                        "{integ.description}"
                                    }
                                    if is_inbox_app(&integ.id) {
                                        Link {
                                            to: Route::InboxApp {
                                                workspace_id: workspace_id.clone(),
                                                provider: integ.id.clone(),
                                            },
                                            class: "mt-2 inline-block text-xs text-[var(--app-accent)] underline",
                                            "Open inbox"
                                        }
                                    }
                                    if let Some(err) = last_error.as_deref() {
                                        if !connected {
                                            p { class: "mt-2 text-xs leading-snug text-red-300/90",
                                                "{err}"
                                            }
                                        }
                                    }
                                    if connected {
                                        IntegrationDefaults {
                                            workspace_id: workspace_id.clone(),
                                            integ: integ.clone(),
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
fn IntegrationDefaults(workspace_id: String, integ: Integration) -> Element {
    use crate::pages::runners::self_hosted::api::{list_workers_page, HostPublic};
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

async fn refresh_until_connected(
    workspace_id: &str,
    provider: &str,
    mut items: Signal<Vec<Integration>>,
    attempts: u32,
    delay_ms: u32,
) -> bool {
    for _ in 0..attempts {
        match list_integrations(workspace_id).await {
            Ok(list) => {
                let done = list
                    .iter()
                    .any(|i| i.id.eq_ignore_ascii_case(provider) && integration_is_connected(i));
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
                        refresh_until_connected(workspace_id, provider, items, 20, 300).await;

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
            let done = list
                .iter()
                .any(|i| i.id.eq_ignore_ascii_case(provider) && integration_is_connected(i));
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
