use dioxus::prelude::*;

use crate::theme::use_theme;

#[component]
pub fn ThemeToggle() -> Element {
    let mut theme = use_theme();

    rsx! {
        button {
            class: "inline-flex items-center gap-2 rounded-lg border border-[var(--app-border)] bg-[var(--app-bg-elevated)] px-3 py-1.5 text-sm font-medium text-[var(--app-fg)] transition hover:bg-[var(--app-bg-hover)]",
            r#type: "button",
            title: "Toggle black / white theme",
            onclick: move |_| {
                let next = theme().toggle();
                theme.set(next);
            },
            span { class: "text-[var(--app-muted)]", "Theme" }
            span {
                class: "rounded-md bg-[var(--app-accent)] px-2 py-0.5 text-xs font-semibold text-[var(--app-accent-fg)]",
                "{theme().label()}"
            }
        }
    }
}
