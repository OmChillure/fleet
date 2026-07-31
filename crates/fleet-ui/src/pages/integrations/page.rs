//! Connect platforms via OAuth (GitHub, Slack, Jira, Linear) — no PAT paste.

use dioxus::prelude::*;

use super::api::{
    Integration, disconnect_integration, list_integrations, oauth_start_url,
};

#[component]
pub fn Integrations(workspace_id: String) -> Element {
    let mut items = use_signal(|| Vec::<Integration>::new());
    let mut loading = use_signal(|| true);
    let mut error = use_signal(|| Option::<String>::None);
    let mut busy_provider = use_signal(|| Option::<String>::None);
    let mut banner = use_signal(|| Option::<(bool, String)>::None);

    // OAuth callback lands here with ?connected=github or ?error=…
    use_effect(move || {
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(window) = web_sys::window() {
                if let Ok(search) = window.location().search() {
                    let q = search.trim_start_matches('?');
                    for pair in q.split('&') {
                        if let Some((k, v)) = pair.split_once('=') {
                            if k == "connected" && !v.is_empty() {
                                banner.set(Some((
                                    true,
                                    format!("{} connected. Agents can use it via the control plane.", url_decode(v)),
                                )));
                            } else if k == "error" && !v.is_empty() {
                                banner.set(Some((
                                    false,
                                    format!("Could not connect: {}", url_decode(v)),
                                )));
                            }
                        }
                    }
                }
            }
        }
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
                    error.set(Some(e.message()));
                }
            }
            loading.set(false);
        });
    });

    rsx! {
        div { class: "flex h-full min-h-[50vh] w-full flex-col",
            div { class: "mb-6",
                h1 { class: "m-0 text-2xl font-bold tracking-tight text-[var(--app-fg)]",
                    "Integrations"
                }
                p { class: "mt-1 max-w-2xl text-sm text-[var(--app-muted)]",
                    "Connect GitHub, Slack, and other tools with OAuth — the same way as any normal web app. Tokens stay on the server so AI can use them later; they never go into sandboxes."
                }
            }

            if let Some((ok, msg)) = banner() {
                div {
                    class: if ok {
                        "mb-4 rounded-xl border border-emerald-500/40 bg-emerald-500/10 px-4 py-3 text-sm text-emerald-300"
                    } else {
                        "mb-4 rounded-xl border border-red-500/40 bg-red-500/10 px-4 py-3 text-sm text-red-300"
                    },
                    "{msg}"
                }
            }

            if loading() {
                div {
                    class: "flex flex-1 items-center justify-center rounded-xl border border-dashed border-[var(--app-border)] bg-[var(--app-bg-elevated)] py-16",
                    p { class: "text-sm text-[var(--app-muted)]", "Loading integrations…" }
                }
            } else if let Some(err) = error() {
                div {
                    class: "rounded-xl border border-red-500/40 bg-red-500/10 px-4 py-3 text-sm text-red-300",
                    "{err}"
                }
            } else {
                div {
                    class: "grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-3",
                    for integ in items() {
                        {
                            let integ_disconnect = integ.clone();
                            let ws_disconnect = workspace_id.clone();
                            let ws_connect = workspace_id.clone();
                            let available = integ.availability == "available";
                            let connected = integ.connected;
                            let configured = integ.oauth_configured;
                            let is_busy = busy_provider().as_deref() == Some(integ.id.as_str());
                            let monogram = integ
                                .name
                                .chars()
                                .next()
                                .map(|c| c.to_uppercase().to_string())
                                .unwrap_or_else(|| "?".into());
                            let account = integ
                                .connection
                                .as_ref()
                                .and_then(|c| c.account_label.clone())
                                .unwrap_or_default();

                            let status_label = if connected {
                                "Connected"
                            } else if !available {
                                "Coming soon"
                            } else if !configured {
                                "Not configured"
                            } else {
                                "Not connected"
                            };
                            let status_class = if connected {
                                "bg-emerald-500/15 text-emerald-400 ring-1 ring-emerald-500/30"
                            } else if !available {
                                "bg-amber-500/10 text-amber-400/90 ring-1 ring-amber-500/25"
                            } else if !configured {
                                "bg-orange-500/10 text-orange-300 ring-1 ring-orange-500/25"
                            } else {
                                "bg-[var(--app-bg-hover)] text-[var(--app-muted)] ring-1 ring-[var(--app-border)]"
                            };
                            let icon_class = match integ.icon.as_str() {
                                "github" => "bg-neutral-800 text-white",
                                "jira" => "bg-blue-600 text-white",
                                "slack" => "bg-purple-600 text-white",
                                "linear" => "bg-indigo-600 text-white",
                                "gitlab" => "bg-orange-600 text-white",
                                "teams" => "bg-sky-600 text-white",
                                _ => "bg-[var(--app-accent)] text-[var(--app-bg)]",
                            };

                            let provider_connect = integ.id.clone();

                            rsx! {
                                article {
                                    key: "{integ.id}",
                                    class: "flex flex-col rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4 transition hover:border-[var(--app-muted)]",
                                    div { class: "flex items-start gap-3",
                                        div {
                                            class: "flex h-11 w-11 shrink-0 items-center justify-center rounded-lg text-sm font-bold {icon_class}",
                                            "{monogram}"
                                        }
                                        div { class: "min-w-0 flex-1",
                                            div { class: "flex flex-wrap items-center gap-2",
                                                h2 { class: "m-0 text-base font-semibold text-[var(--app-fg)]",
                                                    "{integ.name}"
                                                }
                                                span {
                                                    class: "rounded-full px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide {status_class}",
                                                    "{status_label}"
                                                }
                                            }
                                            p { class: "mt-0.5 text-xs font-medium text-[var(--app-muted)]",
                                                "{integ.category_label} · OAuth"
                                            }
                                            if connected && !account.is_empty() {
                                                p { class: "mt-1 truncate text-xs text-[var(--app-fg)]",
                                                    "Account: {account}"
                                                }
                                            }
                                        }
                                    }

                                    p { class: "mt-3 flex-1 text-sm leading-relaxed text-[var(--app-muted)]",
                                        "{integ.description}"
                                    }

                                    if available && !configured {
                                        p { class: "mt-2 text-[11px] leading-snug text-orange-300/90",
                                            "Admin: set {integ.id.to_uppercase()}_CLIENT_ID and {integ.id.to_uppercase()}_CLIENT_SECRET, then restart the API."
                                        }
                                    }

                                    div { class: "mt-4 flex flex-wrap items-center gap-2",
                                        if available && configured && !connected {
                                            button {
                                                class: "rounded-md bg-[var(--app-fg)] px-3 py-1.5 text-xs font-semibold text-[var(--app-bg)] transition hover:opacity-90",
                                                r#type: "button",
                                                onclick: move |_| {
                                                    let url = oauth_start_url(&ws_connect, &provider_connect);
                                                    #[cfg(target_arch = "wasm32")]
                                                    {
                                                        if let Some(window) = web_sys::window() {
                                                            let _ = window.location().set_href(&url);
                                                        }
                                                    }
                                                    #[cfg(not(target_arch = "wasm32"))]
                                                    {
                                                        let _ = url;
                                                    }
                                                },
                                                "Connect with {integ.name}"
                                            }
                                        }
                                        if available && configured && connected {
                                            {
                                                let ws_reconnect = workspace_id.clone();
                                                let provider_reconnect = integ.id.clone();
                                                rsx! {
                                                    button {
                                                        class: "rounded-md border border-[var(--app-border)] px-3 py-1.5 text-xs font-medium text-[var(--app-fg)] transition hover:bg-[var(--app-bg-hover)]",
                                                        r#type: "button",
                                                        onclick: move |_| {
                                                            let url = oauth_start_url(
                                                                &ws_reconnect,
                                                                &provider_reconnect,
                                                            );
                                                            #[cfg(target_arch = "wasm32")]
                                                            {
                                                                if let Some(window) = web_sys::window() {
                                                                    let _ = window.location().set_href(&url);
                                                                }
                                                            }
                                                            #[cfg(not(target_arch = "wasm32"))]
                                                            {
                                                                let _ = url;
                                                            }
                                                        },
                                                        "Reconnect"
                                                    }
                                                }
                                            }
                                            button {
                                                class: "rounded-md px-3 py-1.5 text-xs font-medium text-red-400 transition hover:bg-red-500/10 disabled:opacity-50",
                                                r#type: "button",
                                                disabled: is_busy,
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
                                                                banner.set(Some((true, format!("{provider} disconnected."))));
                                                            }
                                                            Err(e) => {
                                                                banner.set(Some((false, e.message())));
                                                            }
                                                        }
                                                        busy_provider.set(None);
                                                    });
                                                },
                                                if is_busy { "Working…" } else { "Disconnect" }
                                            }
                                        }
                                        if !available {
                                            button {
                                                class: "rounded-md border border-[var(--app-border)] px-3 py-1.5 text-xs font-medium text-[var(--app-muted)]",
                                                r#type: "button",
                                                disabled: true,
                                                "Coming soon"
                                            }
                                        }
                                        if let Some(url) = integ.docs_url.clone() {
                                            a {
                                                class: "rounded-md px-2 py-1.5 text-xs font-medium text-[var(--app-muted)] transition hover:text-[var(--app-fg)]",
                                                href: "{url}",
                                                target: "_blank",
                                                rel: "noopener noreferrer",
                                                "Docs ↗"
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

fn url_decode(s: &str) -> String {
    // Minimal query-value decode for banners.
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = &s[i + 1..i + 3];
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v as char);
                    i += 3;
                } else {
                    out.push('%');
                    i += 1;
                }
            }
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    out
}
