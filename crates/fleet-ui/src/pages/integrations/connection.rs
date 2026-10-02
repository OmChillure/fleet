//! One connection: name, instance URL, sign-in, and disconnect.

use dioxus::prelude::*;

use super::api::{
    ConnectionPublic, Integration, disconnect_connection, list_integrations, live_connections,
    oauth_start_url_setup, rename_connection,
};
use super::logos::IntegrationLogo;
use crate::Route;
use crate::components::toast::show_toast;
use crate::pages::inbox::is_inbox_app;

fn connection_title(conn: &ConnectionPublic, fallback: &str) -> String {
    let name = conn.display_name.trim();
    if name.is_empty() {
        fallback.to_string()
    } else {
        name.to_string()
    }
}

fn instance_url(conn: &ConnectionPublic) -> String {
    conn.config
        .get("base_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string()
}

pub fn needs_instance_url(provider: &str) -> bool {
    provider == "gitea"
}

#[component]
pub fn ConnectionEdit(workspace_id: String, provider: String, connection_id: String) -> Element {
    let route = use_route::<crate::Route>();
    let (workspace_id, provider, connection_id) = match route {
        crate::Route::ConnectionEdit {
            workspace_id,
            provider,
            connection_id,
        } => (workspace_id, provider, connection_id),
        _ => (workspace_id, provider, connection_id),
    };
    let mut row = use_signal(|| None::<(Integration, ConnectionPublic)>);
    let mut missing = use_signal(|| false);
    let mut name = use_signal(String::new);
    let mut url = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let nav = use_navigator();
    let mut bound = use_signal(|| connection_id.clone());
    if bound() != connection_id {
        bound.set(connection_id.clone());
        row.set(None);
        missing.set(false);
        name.set(String::new());
        url.set(String::new());
    }
    let ws = workspace_id.clone();
    let prov = provider.clone();
    use_effect(move || {
        let cid = bound();
        let ws = ws.clone();
        let prov = prov.clone();
        spawn(async move {
            match list_integrations(&ws).await {
                Ok(list) => {
                    if bound() != cid {
                        return;
                    }
                    let found = list.into_iter().find_map(|integ| {
                        if !integ.id.eq_ignore_ascii_case(&prov) {
                            return None;
                        }
                        live_connections(&integ)
                            .into_iter()
                            .find(|c| c.id == cid)
                            .map(|conn| (integ, conn))
                    });
                    if bound() != cid {
                        return;
                    }
                    if let Some((integ, conn)) = found {
                        name.set(connection_title(&conn, &integ.name));
                        url.set(instance_url(&conn));
                        row.set(Some((integ, conn)));
                        missing.set(false);
                    } else {
                        missing.set(true);
                    }
                }
                Err(e) => show_toast().error(e.message()),
            }
        });
    });

    let Some((integ, conn)) = row() else {
        return rsx! {
            div { class: "mx-auto max-w-xl p-5",
                if missing() {
                    p { class: "text-sm text-[var(--app-muted)]", "This connection is not on." }
                } else {
                    p { class: "text-sm text-[var(--app-muted)]", "Loading connection…" }
                }
                Link {
                    to: Route::Integrations { workspace_id: workspace_id.clone() },
                    class: "mt-3 inline-block text-sm text-[var(--app-fg)] underline",
                    "Back to Integrations"
                }
            }
        };
    };
    let failed = conn.status.eq_ignore_ascii_case("error");
    let account = conn.account_label.clone().unwrap_or_default();
    let err = conn.last_error.clone().unwrap_or_default();
    let show_url = needs_instance_url(&integ.id);
    rsx! {
        div { class: "mx-auto flex w-full max-w-xl flex-col gap-4 p-5",
            Link {
                to: Route::Integrations { workspace_id: workspace_id.clone() },
                class: "text-xs text-[var(--app-muted)] no-underline hover:underline",
                "Integrations"
            }
            div { class: "flex items-start gap-3",
                IntegrationLogo { icon: integ.icon.clone(), name: integ.name.clone() }
                div {
                    h1 { class: "m-0 text-2xl font-bold text-[var(--app-fg)]", "{name}" }
                    p { class: "m-0 mt-1 text-sm text-[var(--app-muted)]",
                        "{integ.name}"
                        if !account.is_empty() { " · {account}" }
                    }
                }
            }
            if failed {
                div { class: "rounded-xl border border-red-500/40 bg-red-500/15 px-4 py-3 text-sm text-red-200",
                    p { class: "m-0 font-semibold", "Sign-in failed" }
                    if !err.is_empty() {
                        p { class: "m-0 mt-1", "{err}" }
                    }
                    p { class: "m-0 mt-1", "Reconnect this account. Other connections of {integ.name} stay as they are." }
                }
            }
            label { class: "block text-xs text-[var(--app-muted)]",
                "Name"
                input {
                    class: "mt-1 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                    value: "{name}",
                    oninput: move |e| name.set(e.value()),
                }
            }
            if show_url {
                label { class: "block text-xs text-[var(--app-muted)]",
                    "Gitea URL"
                    input {
                        class: "mt-1 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                        placeholder: "https://gitea.example.com",
                        value: "{url}",
                        oninput: move |e| url.set(e.value()),
                    }
                    p { class: "m-0 mt-1", "Used the next time you reconnect this account." }
                }
            }
            div { class: "flex flex-wrap gap-2",
                button {
                    class: "rounded-lg bg-[var(--app-accent)] px-3 py-2 text-sm font-semibold text-[var(--app-accent-fg)] disabled:opacity-50",
                    r#type: "button",
                    disabled: busy(),
                    onclick: {
                        let ws = workspace_id.clone();
                        let provider = integ.id.clone();
                        let id = conn.id.clone();
                        move |_| {
                            let ws = ws.clone();
                            let provider = provider.clone();
                            let id = id.clone();
                            let next = name().trim().to_string();
                            if next.is_empty() {
                                show_toast().error("Name this connection.");
                                return;
                            }
                            busy.set(true);
                            spawn(async move {
                                match rename_connection(&ws, &provider, &id, &next).await {
                                    Ok(()) => show_toast().success("Name saved."),
                                    Err(e) => show_toast().error(e.message()),
                                }
                                busy.set(false);
                            });
                        }
                    },
                    "Save name"
                }
                button {
                    class: "rounded-lg border border-[var(--app-border)] px-3 py-2 text-sm font-semibold text-[var(--app-fg)]",
                    r#type: "button",
                    onclick: {
                        let ws = workspace_id.clone();
                        let provider = integ.id.clone();
                        move |_| {
                            let title = name().trim().to_string();
                            if title.is_empty() {
                                show_toast().error("Name this connection before reconnecting.");
                                return;
                            }
                            let start = oauth_start_url_setup(&ws, &provider, &title, url().trim());
                            #[cfg(target_arch = "wasm32")]
                            {
                                if let Some(window) = web_sys::window() {
                                    let _ = window.open_with_url_and_target(&start, "fleet_oauth");
                                }
                            }
                            #[cfg(not(target_arch = "wasm32"))]
                            {
                                let _ = start;
                            }
                            show_toast().info("Finish sign-in in the new tab. This page keeps the same connection.");
                        }
                    },
                    if failed { "Reconnect" } else { "Update sign-in" }
                }
                if is_inbox_app(&integ.id) {
                    Link {
                        to: Route::InboxConnection {
                            workspace_id: workspace_id.clone(),
                            provider: integ.id.clone(),
                            connection_id: conn.id.clone(),
                        },
                        class: "inline-flex items-center text-sm text-[var(--app-accent)] underline",
                        "Open inbox"
                    }
                }
                button {
                    class: "rounded-lg border border-red-500/40 px-3 py-2 text-sm font-semibold text-red-300",
                    r#type: "button",
                    disabled: busy(),
                    onclick: {
                        let ws = workspace_id.clone();
                        let provider = integ.id.clone();
                        let id = conn.id.clone();
                        move |_| {
                            let ws = ws.clone();
                            let provider = provider.clone();
                            let id = id.clone();
                            busy.set(true);
                            spawn(async move {
                                match disconnect_connection(&ws, &provider, &id).await {
                                    Ok(()) => {
                                        show_toast().success("Connection removed.");
                                        nav.push(Route::Integrations { workspace_id: ws });
                                    }
                                    Err(e) => show_toast().error(e.message()),
                                }
                                busy.set(false);
                            });
                        }
                    },
                    "Disconnect"
                }
            }
        }
    }
}
