//! Connect this inbox app from its own page (OAuth tab + poll).

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

use crate::components::toast::show_toast;
use crate::pages::integrations::{list_integrations, oauth_start_url};

const OAUTH_POLL_ATTEMPTS: u32 = 90;

#[cfg(target_arch = "wasm32")]
fn open_oauth_tab(url: &str) -> bool {
    let Some(window) = web_sys::window() else {
        return false;
    };
    match window.open_with_url_and_target(url, "fleet_oauth") {
        Ok(Some(tab)) => {
            let _ = tab.focus();
            true
        }
        Ok(None) => false,
        Err(_) => false,
    }
}

#[component]
pub fn ConnectApp(
    workspace_id: String,
    provider: String,
    name: String,
    mut connected: Signal<bool>,
    mut account: Signal<String>,
) -> Element {
    let mut connecting = use_signal(|| false);

    if connected() {
        let label = account();
        return rsx! {
            span { class: "inline-flex items-center gap-1.5 rounded-lg border border-emerald-500/30 bg-emerald-500/10 px-2.5 py-1 text-xs font-semibold text-emerald-300",
                span { class: "h-1.5 w-1.5 rounded-full bg-emerald-400" }
                if label.is_empty() {
                    "Connected"
                } else {
                    "Connected · {label}"
                }
            }
        };
    }

    rsx! {
        button {
            class: "inline-flex min-w-[6.5rem] items-center justify-center gap-1.5 rounded-lg bg-[var(--app-fg)] px-3 py-1.5 text-xs font-semibold text-[var(--app-bg)] disabled:cursor-wait disabled:opacity-80",
            r#type: "button",
            disabled: connecting(),
            title: "Sign in with {name} and grant access",
            onclick: move |_| {
                if connecting() {
                    return;
                }
                let ws = workspace_id.clone();
                let provider = provider.clone();
                let name = name.clone();
                let url = oauth_start_url(&ws, &provider);
                #[cfg(target_arch = "wasm32")]
                {
                    if !open_oauth_tab(&url) {
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
                connecting.set(true);
                show_toast().info(format!(
                    "Authorize {name} in the new tab. This page will update when done."
                ));
                spawn(async move {
                    for _ in 0..OAUTH_POLL_ATTEMPTS {
                        if let Ok(list) = list_integrations(&ws).await {
                            if let Some(i) = list
                                .iter()
                                .find(|i| i.id.eq_ignore_ascii_case(&provider) && i.connected)
                            {
                                connected.set(true);
                                let label = i
                                    .connection
                                    .as_ref()
                                    .and_then(|c| c.account_label.clone())
                                    .unwrap_or_default();
                                account.set(label);
                                connecting.set(false);
                                show_toast().success(format!("{name} connected."));
                                return;
                            }
                        }
                        TimeoutFuture::new(1500).await;
                    }
                    connecting.set(false);
                    show_toast().error(format!(
                        "{name} did not finish connecting. Try again, or open Integrations."
                    ));
                });
            },
            if connecting() {
                span { class: "h-3 w-3 animate-spin rounded-full border-2 border-[var(--app-bg)] border-t-transparent" }
                "Connecting"
            } else {
                "Connect {name}"
            }
        }
    }
}
