use dioxus::prelude::*;

use crate::theme::{Theme, use_theme};

/// Compact sun / moon control for the sidebar footer.
#[component]
pub fn ThemeToggle() -> Element {
    let mut theme = use_theme();
    let is_black = matches!(theme(), Theme::Black);

    rsx! {
        button {
            class: "inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-md border border-[var(--app-border)] bg-[var(--app-bg-elevated)] text-[var(--app-fg)] transition hover:bg-[var(--app-bg-hover)]",
            r#type: "button",
            title: if is_black { "Switch to light theme" } else { "Switch to dark theme" },
            "aria-label": if is_black { "Switch to light theme" } else { "Switch to dark theme" },
            onclick: move |_| {
                let next = theme().toggle();
                theme.set(next);
            },
            // Moon when dark (click → light); sun when light (click → dark)
            if is_black {
                // Moon
                svg {
                    class: "h-4 w-4",
                    view_box: "0 0 24 24",
                    fill: "none",
                    stroke: "currentColor",
                    stroke_width: "1.75",
                    stroke_linecap: "round",
                    stroke_linejoin: "round",
                    path {
                        d: "M21 14.5A8.5 8.5 0 0 1 9.5 3 7 7 0 1 0 21 14.5z",
                    }
                }
            } else {
                // Sun
                svg {
                    class: "h-4 w-4",
                    view_box: "0 0 24 24",
                    fill: "none",
                    stroke: "currentColor",
                    stroke_width: "1.75",
                    stroke_linecap: "round",
                    stroke_linejoin: "round",
                    circle { cx: "12", cy: "12", r: "4" }
                    path { d: "M12 2v2M12 20v2M4.93 4.93l1.41 1.41M17.66 17.66l1.41 1.41M2 12h2M20 12h2M4.93 19.07l1.41-1.41M17.66 6.34l1.41-1.41" }
                }
            }
        }
    }
}
