use dioxus::prelude::*;

/// Shared empty state for app pages — plain header, no nested empty container.
#[component]
pub fn EmptyPage(title: &'static str, description: &'static str) -> Element {
    rsx! {
        div { class: "flex h-full w-full flex-col overflow-auto p-4 md:p-5",
            h1 { class: "m-0 text-2xl font-semibold tracking-tight text-[var(--app-fg)]", "{title}" }
            p { class: "mt-1 text-sm text-[var(--app-muted)]", "{description}" }
        }
    }
}
