use dioxus::prelude::*;

use crate::Route;

#[component]
pub fn Sidebar() -> Element {
    rsx! {
        aside {
            class: "flex h-full w-56 shrink-0 flex-col border-r border-[var(--app-border)] bg-[var(--app-bg)]",
            div { class: "flex h-12 items-center gap-2 border-b border-[var(--app-border)] px-3",
                Link {
                    class: "flex items-center gap-2 font-semibold tracking-tight text-[var(--app-fg)]",
                    to: Route::Home {},
                    div { class: "h-6 w-6 rounded-md bg-[var(--app-accent)]" }
                    span { class: "text-sm", "Agent Fleet" }
                }
            }

            nav { class: "flex flex-1 flex-col gap-0.5 p-2",
                SidebarLink { to: Route::Dashboard {}, label: "Dashboard" }
                SidebarLink { to: Route::Sessions {}, label: "Sessions" }
                SidebarLink { to: Route::Agents {}, label: "Agents" }
                SidebarLink { to: Route::Sandboxes {}, label: "Sandboxes" }
                SidebarLink { to: Route::Settings {}, label: "Settings" }
            }

            div { class: "border-t border-[var(--app-border)] p-2 text-xs text-[var(--app-muted)]",
                Link {
                    class: "block rounded-md px-2 py-1.5 transition hover:bg-[var(--app-bg-hover)] hover:text-[var(--app-fg)]",
                    to: Route::Home {},
                    "← Back to landing"
                }
            }
        }
    }
}

#[component]
fn SidebarLink(to: Route, label: &'static str) -> Element {
    rsx! {
        Link {
            class: "rounded-md px-3 py-2 text-sm font-medium text-[var(--app-muted)] transition hover:bg-[var(--app-bg-hover)] hover:text-[var(--app-fg)]",
            to,
            active_class: "bg-[var(--app-bg-elevated)] text-[var(--app-fg)]",
            "{label}"
        }
    }
}
