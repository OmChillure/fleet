use dioxus::prelude::*;

use crate::components::sidebar::Sidebar;
use crate::pages::auth::api::{AuthError, me};
use crate::theme::Theme;
use crate::Route;

/// App chrome for dashboard and related pages: sidebar + content (no top bar).
///
/// Requires a live session cookie. Uses silent refresh when the short-lived
/// access JWT expired but `fleet_refresh` is still valid.
#[component]
pub fn AppShell() -> Element {
    let theme = use_signal(|| Theme::Black);
    use_context_provider(|| theme);

    let mut auth_state = use_signal(|| AuthGate::Checking);
    let mut gate_message = use_signal(|| Option::<String>::None);
    let navigator = use_navigator();

    use_effect(move || {
        spawn(async move {
            match me().await {
                Ok(_) => {
                    gate_message.set(None);
                    auth_state.set(AuthGate::Ok);
                }
                Err(AuthError::Unauthorized(msg)) => {
                    gate_message.set(Some(msg));
                    auth_state.set(AuthGate::Denied);
                    navigator.replace(Route::Login {});
                }
                Err(e) => {
                    // Network / server blip — do not wipe the session; show retry.
                    gate_message.set(Some(e.message()));
                    auth_state.set(AuthGate::Retryable);
                }
            }
        });
    });

    match auth_state() {
        AuthGate::Checking => rsx! {
            div {
                class: "{theme().root_class()} flex h-screen w-full items-center justify-center bg-[var(--app-bg)] text-[var(--app-muted)]",
                p { class: "text-sm", "Checking session…" }
            }
        },
        AuthGate::Denied => rsx! {
            div {
                class: "{theme().root_class()} flex h-screen w-full items-center justify-center bg-[var(--app-bg)] text-[var(--app-muted)]",
                p { class: "text-sm", "Redirecting to login…" }
            }
        },
        AuthGate::Retryable => rsx! {
            div {
                class: "{theme().root_class()} flex h-screen w-full flex-col items-center justify-center gap-3 bg-[var(--app-bg)] text-[var(--app-fg)]",
                p { class: "text-sm text-[var(--app-muted)]",
                    "Could not reach the API. Your session may still be valid."
                }
                if let Some(msg) = gate_message() {
                    p { class: "max-w-md px-4 text-center text-xs text-red-400", "{msg}" }
                }
                button {
                    class: "rounded-md bg-[var(--app-fg)] px-4 py-2 text-sm font-semibold text-[var(--app-bg)]",
                    r#type: "button",
                    onclick: move |_| {
                        auth_state.set(AuthGate::Checking);
                        gate_message.set(None);
                        spawn(async move {
                            match me().await {
                                Ok(_) => auth_state.set(AuthGate::Ok),
                                Err(AuthError::Unauthorized(_)) => {
                                    auth_state.set(AuthGate::Denied);
                                    navigator.replace(Route::Login {});
                                }
                                Err(e) => {
                                    gate_message.set(Some(e.message()));
                                    auth_state.set(AuthGate::Retryable);
                                }
                            }
                        });
                    },
                    "Retry"
                }
            }
        },
        AuthGate::Ok => rsx! {
            div {
                class: "{theme().root_class()} flex h-screen w-full overflow-hidden bg-[var(--app-sidebar)] text-[var(--app-fg)]",
                Sidebar {}
                main {
                    class: "min-h-0 min-w-0 flex-1 overflow-auto bg-[var(--app-bg)] p-4 md:p-5",
                    Outlet::<crate::Route> {}
                }
            }
        },
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthGate {
    Checking,
    Ok,
    Denied,
    Retryable,
}
