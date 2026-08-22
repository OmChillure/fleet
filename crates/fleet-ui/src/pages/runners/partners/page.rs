//! Partners: other people's unused capacity. Placeholder + trust copy only.

use dioxus::prelude::*;
use fleet_runners_view::placeholders::{
    COMING_SOON_BADGE, DISCLAIMER_DEFAULT_ACCEPTED, PARTNERS_TRUST_COPY,
    disclaimer_ack_enables_lend, disclaimer_ack_makes_assignable,
};
use fleet_runners_view::routes::NAV_LABEL_PARTNERS;

use super::api::{fetch_disclaimer_ack, post_disclaimer_ack};
use crate::components::toast::show_toast;

fn field_class() -> &'static str {
    "mt-1 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)] outline-none disabled:cursor-not-allowed disabled:opacity-50"
}

fn btn_class() -> &'static str {
    "rounded-lg border border-[var(--app-border)] bg-[var(--app-bg-elevated)] px-3 py-2 text-sm font-medium text-[var(--app-fg)] disabled:cursor-not-allowed disabled:opacity-50"
}

/// Coming-soon Partners surface. Ack is a recorded timestamp only.
#[component]
pub fn PartnersPage(workspace_id: String) -> Element {
    let mut acknowledged = use_signal(|| DISCLAIMER_DEFAULT_ACCEPTED);
    let mut posting = use_signal(|| false);

    let ws_load = workspace_id.clone();
    use_effect(move || {
        let ws = ws_load.clone();
        spawn(async move {
            match fetch_disclaimer_ack(&ws).await {
                Ok(ack) => {
                    if ack.accepted {
                        acknowledged.set(true);
                    } else if !posting() {
                        acknowledged.set(false);
                    }
                }
                Err(_) => {
                    if !posting() {
                        acknowledged.set(false);
                    }
                }
            }
        });
    });

    let ack_locked = acknowledged() || posting();

    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-3xl flex-col overflow-auto px-4 py-6 md:px-6",
            div { class: "flex flex-wrap items-start justify-between gap-3",
                div { class: "min-w-0 flex-1",
                    div { class: "flex flex-wrap items-center gap-2",
                        h1 { class: "m-0 text-2xl font-semibold tracking-tight text-[var(--app-fg)]",
                            "{NAV_LABEL_PARTNERS}"
                        }
                        span {
                            class: "inline-flex items-center rounded-full border border-[var(--app-border)] px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)]",
                            "{COMING_SOON_BADGE}"
                        }
                    }
                    p { class: "mt-1 text-sm text-[var(--app-muted)]",
                        "Run work on other people's unused capacity. Coming soon."
                    }
                }
            }

            section {
                class: "mt-6 rounded-2xl border border-amber-500/30 bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                    "Trust"
                }
                ul { class: "mt-2 m-0 list-disc space-y-2 pl-5 text-sm leading-5 text-[var(--app-fg)]",
                    for item in PARTNERS_TRUST_COPY {
                        li { "{item}" }
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
                        disabled: ack_locked,
                        "aria-disabled": if ack_locked { "true" } else { "false" },
                        onchange: {
                            let workspace_id = workspace_id.clone();
                            move |e| {
                                if !e.checked() {
                                    return;
                                }
                                if acknowledged() || posting() {
                                    return;
                                }
                                acknowledged.set(true);
                                posting.set(true);
                                let ws = workspace_id.clone();
                                spawn(async move {
                                    match post_disclaimer_ack(&ws).await {
                                        Ok(ack) => acknowledged.set(ack.accepted),
                                        Err(err) => {
                                            acknowledged.set(false);
                                            show_toast().error(err);
                                        }
                                    }
                                    posting.set(false);
                                });
                            }
                        },
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
                    disabled: !disclaimer_ack_makes_assignable(acknowledged()),
                    "aria-disabled": if disclaimer_ack_makes_assignable(acknowledged()) { "false" } else { "true" },
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
                        disabled: !disclaimer_ack_enables_lend(acknowledged()),
                        "aria-disabled": if disclaimer_ack_enables_lend(acknowledged()) { "false" } else { "true" },
                        "Lend capacity"
                    }
                    button {
                        class: btn_class(),
                        r#type: "button",
                        disabled: !disclaimer_ack_enables_lend(acknowledged()),
                        "aria-disabled": if disclaimer_ack_enables_lend(acknowledged()) { "false" } else { "true" },
                        "Buy credits"
                    }
                }
            }
        }
    }
}
