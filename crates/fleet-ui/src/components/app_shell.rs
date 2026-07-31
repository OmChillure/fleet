use dioxus::prelude::*;

use crate::components::sidebar::Sidebar;
use crate::components::theme_toggle::ThemeToggle;
use crate::pages::auth::api::me;
use crate::theme::Theme;
use crate::Route;

/// App chrome for dashboard and related pages: sidebar + top bar + theme.
///
/// Requires a live session cookie. If `/api/auth/me` fails, redirect to login
/// (the dashboard URL alone is not proof of auth — login JSON can navigate
/// there even when cookies never stuck).
#[component]
pub fn AppShell() -> Element {
    let theme = use_signal(|| Theme::Black);
    use_context_provider(|| theme);

    let mut auth_state = use_signal(|| AuthGate::Checking);
    let navigator = use_navigator();

    use_effect(move || {
        spawn(async move {
            match me().await {
                Ok(_) => auth_state.set(AuthGate::Ok),
                Err(_) => {
                    auth_state.set(AuthGate::Denied);
                    navigator.replace(Route::Login {});
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
        AuthGate::Ok => rsx! {
            div {
                class: "{theme().root_class()} flex h-screen w-full overflow-hidden bg-[var(--app-bg)] text-[var(--app-fg)]",
                Sidebar {}
                div { class: "flex min-w-0 flex-1 flex-col",
                    header {
                        class: "flex h-12 shrink-0 items-center justify-end border-b border-[var(--app-border)] bg-[var(--app-bg)] px-3",
                        ThemeToggle {}
                    }
                    main {
                        class: "min-h-0 flex-1 overflow-auto bg-[var(--app-bg)] p-4",
                        Outlet::<crate::Route> {}
                    }
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
}
