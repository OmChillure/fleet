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

fn control_is_lend(label: &str) -> bool {
    let l = label.to_ascii_lowercase();
    l.contains("lend") || l.contains("buy credit") || l.contains("offer capacity")
}

impl PartnersPageModel {
    fn can_lend(self) -> bool {
        self.controls
            .iter()
            .any(|c| c.enabled && control_is_lend(c.label))
    }

    fn has_balance(self) -> bool {
        self.balance.is_some()
    }

    /// Ack is a timestamp only: no lend, no assignable picker, no balance.
    fn ack_is_not_lend(self) -> bool {
        !self.can_lend() && !self.partner_select_enabled && !self.has_balance()
    }

    fn is_coming_soon(self) -> bool {
        self.badge == COMING_SOON_BADGE && self.subtitle.contains("Coming soon")
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

    fn sample_workspaces() -> [&'static str; 4] {
        [
            "",
            "7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90",
            "partners",
            "c0ffee00-0000-4000-8000-00000000lend",
        ]
    }

    #[test]
    fn partners_page_is_coming_soon_with_trust_copy() {
        let model = partners_page_model("7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90", false);
        assert_eq!(model.heading, "Partners");
        assert_eq!(model.heading, NAV_LABEL_PARTNERS);
        assert_eq!(COMING_SOON_BADGE, "Coming soon");
        assert_eq!(model.badge, COMING_SOON_BADGE);
        assert_eq!(model.badge, "Coming soon");
        assert!(
            model.is_coming_soon(),
            "Partners heading must stay Coming soon"
        );
        assert!(
            model.subtitle.contains("Coming soon"),
            "subtitle must say Coming soon: {}",
            model.subtitle
        );
        assert_eq!(model.trust_heading, "Trust");
        assert_eq!(model.trust_copy, PARTNERS_TRUST_COPY);
        assert_eq!(model.trust_copy.len(), 4);
        assert_eq!(
            model.trust_copy[0],
            "Work and source leave your trust boundary."
        );
        assert_eq!(
            model.trust_copy[1],
            "Fleet is not responsible for what a partner does with it."
        );
        assert!(
            model.trust_copy[2].contains("Verified vs open"),
            "{}",
            model.trust_copy[2]
        );
        assert!(
            model.trust_copy[3].contains("0.9 per 1.0")
                && model.trust_copy[3].contains("no cash-out"),
            "{}",
            model.trust_copy[3]
        );
    }

    #[test]
    fn partners_page_disclaimer_says_ack_is_not_lend() {
        let model = partners_page_model("7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90", true);
        assert_eq!(model.disclaimer_heading, "Disclaimer");
        assert_eq!(
            model.disclaimer_ack_label,
            "I acknowledge the trust terms above. Acknowledging does not enable lending."
        );
        assert!(
            model
                .disclaimer_ack_label
                .to_ascii_lowercase()
                .contains("does not enable lending"),
            "{}",
            model.disclaimer_ack_label
        );
        assert_eq!(
            model.lend_unavailable,
            "Lending and buying credits are not available yet."
        );
        assert!(
            !DISCLAIMER_DEFAULT_ACCEPTED,
            "disclaimer must start unaccepted"
        );
    }

    #[test]
    fn partners_page_ack_does_not_enable_lend_or_balance() {
        assert!(!disclaimer_ack_enables_lend(false));
        assert!(!disclaimer_ack_enables_lend(true));
        assert!(!disclaimer_ack_makes_assignable(false));
        assert!(!disclaimer_ack_makes_assignable(true));
        for acknowledged in [false, true] {
            let model = partners_page_model("7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90", acknowledged);
            assert!(
                model.ack_is_not_lend(),
                "ack={acknowledged} is a timestamp only and must not enable lend"
            );
            assert!(!model.can_lend());
            assert!(!model.has_balance());
            assert!(
                model.balance.is_none(),
                "ack={acknowledged} must not create a balance"
            );
            assert!(
                !model.partner_select_enabled,
                "ack={acknowledged} must not make the partner picker assignable"
            );
        }
    }

    #[test]
    fn partners_page_lend_and_buy_exist_but_stay_disabled() {
        for acknowledged in [false, true] {
            let model = partners_page_model("7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90", acknowledged);
            let labels: Vec<&str> = model.controls.iter().map(|c| c.label).collect();
            assert_eq!(labels, [LEND_CAPACITY_LABEL, BUY_CREDITS_LABEL]);
            assert_eq!(labels, ["Lend capacity", "Buy credits"]);
            assert!(
                model.controls.iter().all(|c| !c.enabled),
                "ack={acknowledged} must keep lend/buy disabled: {:?}",
                model.controls
            );
            assert!(model.controls.iter().all(|c| control_is_lend(c.label)));
        }
    }

    #[test]
    fn partners_page_workspace_id_does_not_change_surface() {
        let baseline = partners_page_model("7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90", false);
        for ws in sample_workspaces() {
            for acknowledged in [false, true] {
                let model = partners_page_model(ws, acknowledged);
                assert_eq!(
                    model, baseline,
                    "workspace {ws:?} ack={acknowledged} must not change the Partners placeholder"
                );
                assert!(model.is_coming_soon());
                assert!(model.ack_is_not_lend());
                assert!(!model.can_lend());
                assert!(!model.has_balance());
            }
        }
    }

    #[test]
    fn partners_page_enabled_lend_breaks_ack_is_not_lend() {
        let model = partners_page_model("7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90", true);
        let mut controls = model.controls;
        controls[0].enabled = true;
        let broken = PartnersPageModel { controls, ..model };
        assert!(broken.can_lend());
        assert!(
            !broken.ack_is_not_lend(),
            "an enabled Lend capacity control must fail the ack-is-not-lend contract"
        );
    }

    #[test]
    fn partners_page_showing_balance_breaks_ack_is_not_lend() {
        let model = partners_page_model("7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90", true);
        let broken = PartnersPageModel {
            balance: Some("12.0"),
            ..model
        };
        assert!(broken.has_balance());
        assert!(
            !broken.can_lend(),
            "a balance is not the same as an enabled lend button"
        );
        assert!(
            !broken.ack_is_not_lend(),
            "a live balance must fail the ack-is-not-lend contract"
        );
    }

    #[test]
    fn partners_page_enabling_select_breaks_ack_is_not_lend() {
        let model = partners_page_model("7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90", true);
        let broken = PartnersPageModel {
            partner_select_enabled: true,
            ..model
        };
        assert!(
            !broken.ack_is_not_lend(),
            "an enabled partner picker must fail the ack-is-not-lend contract"
        );
        assert!(!broken.can_lend());
        assert!(!broken.has_balance());
    }

    #[test]
    fn partners_page_dropping_coming_soon_badge() {
        let model = partners_page_model("7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90", false);
        let broken = PartnersPageModel {
            badge: "Ready",
            ..model
        };
        assert!(
            !broken.is_coming_soon(),
            "Partners without Coming soon must not stay a coming-soon surface"
        );
        let empty = PartnersPageModel { badge: "", ..model };
        assert!(!empty.is_coming_soon());
    }

    fn production_src() -> &'static str {
        include_str!("page.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("partners page source")
    }

    fn live_component_src(prod: &str) -> &str {
        let idx = prod
            .find("#[component]")
            .expect("PartnersPage live component");
        &prod[idx..]
    }

    fn rsx_blocks(src: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut rest = src;
        while let Some(i) = rest.find("rsx!") {
            let after = rest[i + 4..].trim_start();
            let Some(after) = after.strip_prefix('{') else {
                rest = &rest[i + 4..];
                continue;
            };
            let bytes = after.as_bytes();
            let mut depth = 1;
            let mut end = 0;
            for (j, &b) in bytes.iter().enumerate() {
                match b {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = j;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            assert!(end > 0, "unbalanced rsx! block");
            out.push(&after[..end]);
            rest = &after[end..];
        }
        out
    }

    fn rsx_element_blocks<'a>(rsx: &'a str, tag: &str) -> Vec<&'a str> {
        let mut out = Vec::new();
        let mut rest = rsx;
        let needle = tag.to_ascii_lowercase();
        while let Some(i) = rest.to_ascii_lowercase().find(&needle) {
            let after_kw = &rest[i + needle.len()..];
            let trimmed = after_kw.trim_start();
            if !trimmed.starts_with('{') {
                rest = after_kw;
                continue;
            }
            let inner = &trimmed[1..];
            let bytes = inner.as_bytes();
            let mut depth = 1;
            let mut end = 0;
            for (j, &b) in bytes.iter().enumerate() {
                match b {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            end = j;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            assert!(end > 0, "unbalanced {tag} block");
            out.push(&inner[..end]);
            rest = &inner[end..];
        }
        out
    }

    /// Live RSX must stay model-wired. Mutation tests of a hand-built
    /// PartnersPageModel are not enough: adding Spawn/Lend outside the
    /// model, or `disabled: !acknowledged()`, must fail this lock.
    #[test]
    fn partners_page_source_cannot_enable_lend_or_show_balance() {
        let prod = production_src();
        assert!(
            prod.contains("disclaimer_ack_enables_lend"),
            "lend buttons must stay wired through disclaimer_ack_enables_lend"
        );
        assert!(
            prod.contains("disclaimer_ack_makes_assignable"),
            "partner picker must stay wired through disclaimer_ack_makes_assignable"
        );
        assert!(prod.contains("COMING_SOON_BADGE"));
        assert!(prod.contains("PARTNERS_TRUST_COPY"));
        assert!(
            prod.contains("let model = partners_page_model(&workspace_id, acknowledged())"),
            "PartnersPage must render through partners_page_model"
        );
        assert!(
            prod.contains("balance: None"),
            "partners page source must keep balance off"
        );
        assert!(
            !prod.contains("balance: Some"),
            "partners page source must not enable a live balance"
        );
        assert!(
            !prod.contains("disabled: false"),
            "partners page source must not hard-enable lend or assign"
        );

        let live = live_component_src(prod);
        let blocks = rsx_blocks(live);
        assert!(
            !blocks.is_empty(),
            "production Partners page must contain rsx! markup"
        );
        let rsx = blocks.join("\n");

        assert!(
            rsx.contains("{model.badge}"),
            "live RSX must render {{model.badge}}, not a hardcoded badge"
        );
        assert!(
            rsx.contains("{model.subtitle}"),
            "live RSX must render {{model.subtitle}}, not a hardcoded subtitle"
        );
        assert!(
            rsx.contains("for item in model.trust_copy"),
            "live RSX must render {{model.trust_copy}}, not hardcoded trust bullets"
        );
        assert!(
            rsx.contains("for control in model.controls"),
            "live RSX must render buttons only from model.controls"
        );
        assert!(
            rsx.contains("disabled: !control.enabled"),
            "live RSX buttons must stay disabled: !control.enabled (not acknowledged())"
        );
        assert!(
            rsx.contains("disabled: !model.partner_select_enabled"),
            "live RSX must keep disabled: !model.partner_select_enabled"
        );
        assert!(
            rsx.contains("if let Some(amount) = model.balance"),
            "live RSX must keep if let Some(amount) = model.balance"
        );

        let buttons = rsx_element_blocks(&rsx, "button");
        assert_eq!(
            buttons.len(),
            1,
            "PartnersPage must render buttons only from the model.controls loop, got {}",
            buttons.len()
        );
        assert!(
            buttons[0].contains("disabled: !control.enabled"),
            "the only Partners button must use disabled: !control.enabled"
        );
        assert!(
            buttons[0].contains("{control.label}"),
            "the only Partners button must take its label from model.controls"
        );
        let button_l = buttons[0].to_ascii_lowercase();
        assert!(
            !button_l.contains("onclick"),
            "Partners buttons must not onclick lend"
        );
        assert!(
            !button_l.contains("disabled: !acknowledged"),
            "ack must not enable the lend/buy buttons"
        );

        let rsx_l = rsx.to_ascii_lowercase();
        assert!(
            !rsx_l.contains("onclick"),
            "live RSX must contain no onclick lend (or any onclick action)"
        );
        assert!(
            !rsx_l.contains("disabled: !acknowledged"),
            "live RSX must not wire disabled to acknowledged()"
        );
        let spawn_without_async = rsx_l.replace("spawn(", "");
        assert!(
            !spawn_without_async.contains("spawn"),
            "live RSX must contain no Spawn control"
        );
        for needle in [
            "\"spawn\"",
            "'spawn'",
            "spawn worker",
            "spawn capacity",
            "add worker",
        ] {
            assert!(!rsx_l.contains(needle), "live RSX must not grow {needle:?}");
        }
    }
}
