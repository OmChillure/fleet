//! Partners: other people's unused capacity. Placeholder + trust copy only.

use dioxus::prelude::*;

fn field_class() -> &'static str {
    "mt-1 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)] outline-none disabled:cursor-not-allowed disabled:opacity-50"
}

fn btn_class() -> &'static str {
    "rounded-lg border border-[var(--app-border)] bg-[var(--app-bg-elevated)] px-3 py-2 text-sm font-medium text-[var(--app-fg)] disabled:cursor-not-allowed disabled:opacity-50"
}

/// Coming-soon Partners surface. Checkbox is local UI state only.
#[component]
pub fn PartnersPage(workspace_id: String) -> Element {
    let mut acknowledged = use_signal(|| false);
    let _ = workspace_id;

    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-3xl flex-col overflow-auto px-4 py-6 md:px-6",
            div { class: "flex flex-wrap items-start justify-between gap-3",
                div { class: "min-w-0 flex-1",
                    div { class: "flex flex-wrap items-center gap-2",
                        h1 { class: "m-0 text-2xl font-semibold tracking-tight text-[var(--app-fg)]",
                            "Partners"
                        }
                        span {
                            class: "inline-flex items-center rounded-full border border-[var(--app-border)] px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)]",
                            "Coming soon"
                        }
                    }
                    p { class: "mt-1 text-sm text-[var(--app-muted)]",
                        "Run work on other people's unused capacity. Isolation: Sandboxed."
                    }
                }
            }

            section {
                class: "mt-6 rounded-2xl border border-amber-500/30 bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                    "Trust"
                }
                ul { class: "mt-2 m-0 list-disc space-y-2 pl-5 text-sm leading-5 text-[var(--app-fg)]",
                    li { "Work and source leave your trust boundary." }
                    li { "Fleet is not responsible for what a partner does with it." }
                    li { "Verified vs open. Pick a partner, any verified, or any." }
                    li {
                        "Credits: earn by lending; buy credits; 0.9 per 1.0 generated (0.1 Fleet). Money-out is later; no cash-out button."
                    }
                }
            }

            section { class: "mt-4 rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                    "Disclaimer"
                }
                label { class: "mt-3 flex items-start gap-2 text-sm leading-5 text-[var(--app-fg)]",
                    input {
                        class: "mt-0.5",
                        r#type: "checkbox",
                        checked: acknowledged(),
                        onchange: move |e| acknowledged.set(e.checked()),
                    }
                    span {
                        "I acknowledge the trust terms above. Acknowledging does not enable lending."
                    }
                }
            }

            section { class: "mt-4 rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                    "Partner"
                }
                p { class: "mt-1 text-xs text-[var(--app-muted)]",
                    "Choose a named partner, any verified partner, or any open partner."
                }
                label { class: "mt-3 block text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)]",
                    "Send work to"
                }
                select {
                    class: field_class(),
                    disabled: true,
                    "aria-disabled": "true",
                    option { value: "", selected: true, "Pick a partner" }
                    option { value: "verified", "Any verified partner" }
                    option { value: "any", "Any partner" }
                }
            }

            section { class: "mt-4 rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                    "Lend"
                }
                p { class: "mt-1 text-xs text-[var(--app-muted)]",
                    "Lending and buying credits are not available yet."
                }
                div { class: "mt-3 flex flex-wrap items-center gap-2",
                    button {
                        class: btn_class(),
                        r#type: "button",
                        disabled: true,
                        "aria-disabled": "true",
                        "Lend capacity"
                    }
                    button {
                        class: btn_class(),
                        r#type: "button",
                        disabled: true,
                        "aria-disabled": "true",
                        "Buy credits"
                    }
                }
            }
        }
    }
}
