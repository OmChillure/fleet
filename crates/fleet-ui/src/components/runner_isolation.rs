//! Isolation chip: Native (v1) vs Sandboxed (executor not in v1).
//!
//! A1/A2 mount this later; allow unused until then.

#![allow(dead_code)]

use dioxus::prelude::*;

/// How a runner executes jobs. v1 is native-only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum IsolationMode {
    #[default]
    Native,
    Sandboxed,
}

impl IsolationMode {
    pub fn as_label(self) -> &'static str {
        match self {
            Self::Native => "Native",
            Self::Sandboxed => "Sandboxed",
        }
    }

    /// Sandboxed is shown but not a working executor in v1.
    pub fn is_enabled(self) -> bool {
        matches!(self, Self::Native)
    }
}

const V1_MODE: IsolationMode = IsolationMode::Native;
const SANDBOXED_HINT: &str = "Executor is not in v1.";

/// Isolation: Native | Sandboxed. Native is selected. Sandboxed is disabled.
///
/// Optional `caption` is display-only (no API calls, no k8s).
#[component]
pub fn RunnerIsolationChip(caption: Option<String>) -> Element {
    let caption = caption
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    rsx! {
        div {
            class: "inline-flex flex-col gap-1",
            div {
                class: "inline-flex flex-wrap items-center gap-2",
                role: "group",
                "aria-label": "Isolation",
                span {
                    class: "text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)]",
                    "Isolation"
                }
                div {
                    class: "inline-flex items-center rounded-full border border-[var(--app-border)] bg-[var(--app-bg)] p-0.5",
                    IsolationOption { mode: IsolationMode::Native, selected: V1_MODE == IsolationMode::Native }
                    IsolationOption { mode: IsolationMode::Sandboxed, selected: V1_MODE == IsolationMode::Sandboxed }
                }
                if let Some(caption) = caption {
                    span { class: "text-xs text-[var(--app-muted)]", "{caption}" }
                }
            }
            p {
                class: "m-0 text-[10px] leading-4 text-[var(--app-muted)]",
                "{SANDBOXED_HINT}"
            }
        }
    }
}

#[component]
fn IsolationOption(mode: IsolationMode, selected: bool) -> Element {
    let enabled = mode.is_enabled();
    let label = mode.as_label();
    let class = if selected {
        "inline-flex items-center rounded-full bg-[var(--app-fg)] px-2.5 py-0.5 text-xs font-medium text-[var(--app-bg)]"
    } else if !enabled {
        "inline-flex cursor-not-allowed items-center rounded-full px-2.5 py-0.5 text-xs font-medium text-[var(--app-muted)] opacity-60"
    } else {
        "inline-flex items-center rounded-full px-2.5 py-0.5 text-xs font-medium text-[var(--app-muted)]"
    };

    rsx! {
        button {
            class: "{class}",
            r#type: "button",
            disabled: !enabled,
            title: if enabled { "" } else { SANDBOXED_HINT },
            "aria-pressed": selected,
            "aria-disabled": !enabled,
            "{label}"
        }
    }
}
