//! Right-hand detail drawer. Overlay on small screens.

use dioxus::prelude::*;

#[component]
pub fn SidePanel(title: String, on_close: EventHandler<MouseEvent>, children: Element) -> Element {
    rsx! {
        div { class: "fixed inset-0 z-40 flex justify-end md:static md:z-auto md:contents",
            button {
                class: "absolute inset-0 bg-black/40 md:hidden",
                r#type: "button",
                "aria-label": "Close panel",
                onclick: move |e| on_close.call(e),
            }
            aside {
                class: "relative z-10 flex h-full w-full max-w-none flex-col overflow-hidden border-l border-[var(--app-border)] bg-[var(--app-bg-elevated)] shadow-xl md:w-[24rem] md:max-w-[24rem] md:shrink-0 md:shadow-none",
                div { class: "flex items-start justify-between gap-2 border-b border-[var(--app-border)] px-4 py-3",
                    h2 { class: "m-0 min-w-0 flex-1 text-sm font-semibold text-[var(--app-fg)]",
                        "{title}"
                    }
                    button {
                        class: "rounded-lg border border-[var(--app-border)] px-2 py-1 text-xs text-[var(--app-muted)] hover:bg-[var(--app-bg-hover)] hover:text-[var(--app-fg)]",
                        r#type: "button",
                        "aria-label": "Close",
                        onclick: move |e| on_close.call(e),
                        "Close"
                    }
                }
                div { class: "flex min-h-0 flex-1 flex-col",
                    {children}
                }
            }
        }
    }
}
