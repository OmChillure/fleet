use dioxus::prelude::*;

/// Shared empty state for app pages (no mock data yet).
#[component]
pub fn EmptyPage(title: &'static str, description: &'static str) -> Element {
    rsx! {
        div { class: "flex h-full min-h-[50vh] w-full flex-col",
            h1 { class: "m-0 text-2xl font-bold tracking-tight text-[var(--app-fg)]", "{title}" }
            p { class: "mt-1 mb-6 text-sm text-[var(--app-muted)]", "{description}" }
            div {
                class: "flex flex-1 items-center justify-center rounded-xl border border-dashed border-[var(--app-border)] bg-[var(--app-bg-elevated)]",
                p { class: "text-sm text-[var(--app-muted)]", "No data yet" }
            }
        }
    }
}
