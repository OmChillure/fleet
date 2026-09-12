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

const PARTNERS_SUBTITLE: &str = "Run work on other people's unused capacity. Coming soon.";
const PARTNERS_TRUST_HEADING: &str = "Trust";
const PARTNERS_DISCLAIMER_HEADING: &str = "Disclaimer";
const PARTNERS_DISCLAIMER_ACK_LABEL: &str =
    "I acknowledge the trust terms above. Acknowledging does not enable lending.";
const PARTNERS_PARTNER_HEADING: &str = "Partner";
const PARTNERS_PARTNER_HINT: &str =
    "Choose a named partner, any verified partner, or any open partner.";
const PARTNERS_LEND_HEADING: &str = "Lend";
const PARTNERS_LEND_UNAVAILABLE: &str = "Lending and buying credits are not available yet.";
const LEND_CAPACITY_LABEL: &str = "Lend capacity";
const BUY_CREDITS_LABEL: &str = "Buy credits";

/// Interactive control the Partners page might expose (lend / buy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PartnersPageControl {
    label: &'static str,
    enabled: bool,
}

/// Render model for `/{workspace_id}/runners/partners`.
/// Workspace id does not change the surface. Ack is a timestamp only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PartnersPageModel {
    heading: &'static str,
    badge: &'static str,
    subtitle: &'static str,
    trust_heading: &'static str,
    trust_copy: &'static [&'static str],
    disclaimer_heading: &'static str,
    disclaimer_ack_label: &'static str,
    partner_heading: &'static str,
    partner_hint: &'static str,
    partner_select_enabled: bool,
    lend_heading: &'static str,
    lend_unavailable: &'static str,
    /// Live credit balance. Must stay None — ack does not create one.
    balance: Option<&'static str>,
    controls: [PartnersPageControl; 2],
}

/// Coming soon + trust copy. `acknowledged` records a timestamp only.
fn partners_page_model(_workspace_id: &str, acknowledged: bool) -> PartnersPageModel {
    PartnersPageModel {
        heading: NAV_LABEL_PARTNERS,
        badge: COMING_SOON_BADGE,
        subtitle: PARTNERS_SUBTITLE,
        trust_heading: PARTNERS_TRUST_HEADING,
        trust_copy: PARTNERS_TRUST_COPY,
        disclaimer_heading: PARTNERS_DISCLAIMER_HEADING,
        disclaimer_ack_label: PARTNERS_DISCLAIMER_ACK_LABEL,
        partner_heading: PARTNERS_PARTNER_HEADING,
        partner_hint: PARTNERS_PARTNER_HINT,
        partner_select_enabled: disclaimer_ack_makes_assignable(acknowledged),
        lend_heading: PARTNERS_LEND_HEADING,
        lend_unavailable: PARTNERS_LEND_UNAVAILABLE,
        balance: None,
        controls: [
            PartnersPageControl {
                label: LEND_CAPACITY_LABEL,
                enabled: disclaimer_ack_enables_lend(acknowledged),
            },
            PartnersPageControl {
                label: BUY_CREDITS_LABEL,
                enabled: disclaimer_ack_enables_lend(acknowledged),
            },
        ],
    }
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
    let model = partners_page_model(&workspace_id, acknowledged());

    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-3xl flex-col overflow-auto px-4 py-6 md:px-6",
            div { class: "flex flex-wrap items-start justify-between gap-3",
                div { class: "min-w-0 flex-1",
                    div { class: "flex flex-wrap items-center gap-2",
                        h1 { class: "m-0 text-2xl font-semibold tracking-tight text-[var(--app-fg)]",
                            "{model.heading}"
                        }
                        span {
                            class: "inline-flex items-center rounded-full border border-[var(--app-border)] px-2 py-0.5 text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)]",
                            "{model.badge}"
                        }
                    }
                    p { class: "mt-1 text-sm text-[var(--app-muted)]",
                        "{model.subtitle}"
                    }
                }
            }

            section {
                class: "mt-6 rounded-2xl border border-amber-500/30 bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                    "{model.trust_heading}"
                }
                ul { class: "mt-2 m-0 list-disc space-y-2 pl-5 text-sm leading-5 text-[var(--app-fg)]",
                    for item in model.trust_copy {
                        li { "{item}" }
                    }
                }
            }

            section { class: "mt-4 rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                    "{model.disclaimer_heading}"
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
                        "{model.disclaimer_ack_label}"
                    }
                }
            }

            section { class: "mt-4 rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                    "{model.partner_heading}"
                }
                p { class: "mt-1 text-xs text-[var(--app-muted)]",
                    "{model.partner_hint}"
                }
                label { class: "mt-3 block text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)]",
                    "Send work to"
                }
                select {
                    class: field_class(),
                    disabled: !model.partner_select_enabled,
                    "aria-disabled": if model.partner_select_enabled { "false" } else { "true" },
                    option { value: "", selected: true, "Pick a partner" }
                    option { value: "verified", "Any verified partner" }
                    option { value: "any", "Any partner" }
                }
            }

            section { class: "mt-4 rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                    "{model.lend_heading}"
                }
                p { class: "mt-1 text-xs text-[var(--app-muted)]",
                    "{model.lend_unavailable}"
                }
                if let Some(amount) = model.balance {
                    p { class: "mt-1 text-sm text-[var(--app-fg)]",
                        "Balance: {amount}"
                    }
                }
                div { class: "mt-3 flex flex-wrap items-center gap-2",
                    for control in model.controls {
                        button {
                            class: btn_class(),
                            r#type: "button",
                            disabled: !control.enabled,
                            "aria-disabled": if control.enabled { "false" } else { "true" },
                            "{control.label}"
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // catalog:unit-partners-page — Coming soon + trust copy; ack is a
    // timestamp only and must not enable lend, assignable, or a balance.

    #[test]
    fn partners_page_lend_and_buy_exist_but_stay_disabled() {
        for acknowledged in [false, true] {
            let model = partners_page_model("7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90", acknowledged);
            assert_eq!(
                model.controls.map(|c| c.enabled),
                [false, false],
                "ack={acknowledged} must keep both factory lend/buy controls disabled: {:?}",
                model.controls
            );
        }
    }
}
