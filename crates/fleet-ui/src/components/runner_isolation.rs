//! Per-runner Native / Sandboxed checkboxes. Both off = not taking new jobs.

use dioxus::prelude::*;
use fleet_runners_view::{ISOLATION_LABEL, SANDBOXED_HINT};

/// Native and Sandboxed on one runner. Either, both, or neither.
/// Control plane: pass `lock_native` so Native stays off and disabled.
#[component]
pub fn RunnerModeChecks(
    native: bool,
    sandboxed: bool,
    disabled: bool,
    on_native: EventHandler<bool>,
    on_sandboxed: EventHandler<bool>,
    #[props(default = false)] lock_native: bool,
) -> Element {
    rsx! {
        div {
            class: "flex flex-col gap-1",
            span {
                class: "text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)]",
                "{ISOLATION_LABEL}"
            }
            div {
                class: "flex flex-wrap items-center gap-x-4 gap-y-1",
                "aria-label": "Isolation",
                label {
                    class: if lock_native {
                        "inline-flex cursor-not-allowed items-center gap-1.5 text-xs text-[var(--app-muted)] opacity-50"
                    } else {
                        "inline-flex items-center gap-1.5 text-xs text-[var(--app-fg)]"
                    },
                    title: if lock_native {
                        "The control plane does not run native host jobs."
                    } else {
                        "Jobs run as processes on this machine."
                    },
                    input {
                        r#type: "checkbox",
                        class: if lock_native { "cursor-not-allowed accent-[var(--app-muted)]" } else { "" },
                        checked: !lock_native && native,
                        disabled: disabled || lock_native,
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
                    span { class: "font-normal text-[var(--app-muted)]",
                        "* Sandboxed is not implemented yet."
                    }
                }
            }
        }
    }
}
