use dioxus::prelude::*;

use crate::components::sidebar::Sidebar;
use crate::components::theme_toggle::ThemeToggle;
use crate::theme::Theme;

/// App chrome for dashboard and related pages: sidebar + top bar + theme.
#[component]
pub fn AppShell() -> Element {
    let theme = use_signal(|| Theme::Black);
    use_context_provider(|| theme);

    rsx! {
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
    }
}
