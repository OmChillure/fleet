//! Per-runner Native / Sandboxed checkboxes. Both off = not taking new jobs.

use dioxus::prelude::*;

const SANDBOXED_HINT: &str = "Sandboxed is not implemented yet.";

/// Native and Sandboxed on one runner. Either, both, or neither.
#[component]
pub fn RunnerModeChecks(
    native: bool,
    sandboxed: bool,
    disabled: bool,
    on_native: EventHandler<bool>,
    on_sandboxed: EventHandler<bool>,
) -> Element {
    rsx! {
        div {
            class: "flex flex-col gap-1",
            div {
                class: "flex flex-wrap items-center gap-x-4 gap-y-1",
                "aria-label": "What this runner accepts",
                label {
                    class: "inline-flex items-center gap-1.5 text-xs text-[var(--app-fg)]",
                    title: "Jobs run as processes on this machine.",
                    input {
                        r#type: "checkbox",
                        checked: native,
                        disabled,
                        onchange: move |e| on_native.call(e.checked()),
                    }
                    "Native"
                }
                label {
                    class: "inline-flex items-center gap-1.5 text-xs text-[var(--app-fg)]",
                    title: SANDBOXED_HINT,
                    input {
                        r#type: "checkbox",
                        checked: sandboxed,
                        disabled,
                        onchange: move |e| on_sandboxed.call(e.checked()),
                    }
                    "Sandboxed"
                }
            }
            p { class: "m-0 text-[10px] leading-4 text-[var(--app-muted)]",
                "Sandboxed is not implemented yet."
            }
        }
    }
}
