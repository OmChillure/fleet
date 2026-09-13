//! Cloud / Partners copy (extracted from fleet-ui).
//!
//! Cloud BYO cards stay labeled coming soon; Fleet Cloud attach is live.
//! Partners disclaimer ack unlocks lend / borrow UI. It does not mint
//! credits or make a host assignable by itself.

/// Badge on every Cloud card and the Partners heading.
pub const COMING_SOON_BADGE: &str = "Coming soon";

/// One Cloud placeholder card (AWS / GCP / Azure / Fleet Cloud).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloudCardCopy {
    pub name: &'static str,
    pub body: &'static str,
}

impl CloudCardCopy {
    /// Every Cloud card is coming soon.
    pub const fn badge(self) -> &'static str {
        COMING_SOON_BADGE
    }
}

pub const CLOUD_CARD_AWS: CloudCardCopy = CloudCardCopy {
    name: "AWS",
    body: "Bring-your-own AWS. Customer keys stay in the workspace; secrets never enter a sandbox.",
};

pub const CLOUD_CARD_GCP: CloudCardCopy = CloudCardCopy {
    name: "GCP",
    body: "Bring-your-own GCP. Customer keys stay in the workspace; secrets never enter a sandbox.",
};

pub const CLOUD_CARD_AZURE: CloudCardCopy = CloudCardCopy {
    name: "Azure",
    body:
        "Bring-your-own Azure. Customer keys stay in the workspace; secrets never enter a sandbox.",
};

pub const CLOUD_CARD_FLEET: CloudCardCopy = CloudCardCopy {
    name: "Fleet Cloud",
    body: "Paid pool of Fleet-hosted runners. You do not bring cloud keys.",
};

/// AWS, GCP, Azure, Fleet Cloud — in page order.
pub const CLOUD_CARDS: [CloudCardCopy; 4] = [
    CLOUD_CARD_AWS,
    CLOUD_CARD_GCP,
    CLOUD_CARD_AZURE,
    CLOUD_CARD_FLEET,
];

/// Trust bullets on the Partners page.
pub const PARTNERS_TRUST_COPY: &[&str] = &[
    "Work and source leave your trust boundary.",
    "Fleet is not responsible for what a partner does with it.",
    "Verified vs open. Pick a partner, any verified, or any.",
    "Credits: earn by lending; buy credits; 0.9 per 1.0 generated (0.1 Fleet). Money-out is later; no cash-out button.",
];

/// Disclaimer checkbox starts unchecked (no timestamp recorded).
pub const DISCLAIMER_DEFAULT_ACCEPTED: bool = false;

/// Recording an ack unlocks the lend / borrow controls. Buy credits stays off.
pub fn disclaimer_ack_enables_lend(accepted: bool) -> bool {
    accepted
}

/// Recording an ack does not make a partner host assignable.
pub fn disclaimer_ack_makes_assignable(_accepted: bool) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // catalog:unit-cloud-partners-helpers — Cloud/Partners page render
    // helpers: four Coming soon cards; ack is not lend/assignable.

    #[test]
    fn cloud_has_four_named_cards() {
        assert_eq!(CLOUD_CARDS.len(), 4);
        assert_eq!(
            CLOUD_CARDS,
            [
                CLOUD_CARD_AWS,
                CLOUD_CARD_GCP,
                CLOUD_CARD_AZURE,
                CLOUD_CARD_FLEET,
            ]
        );
        let names: Vec<&str> = CLOUD_CARDS.iter().map(|c| c.name).collect();
        assert_eq!(names, ["AWS", "GCP", "Azure", "Fleet Cloud"]);
    }

    #[test]
    fn cloud_card_bodies_are_placeholders_not_actions() {
        for card in CLOUD_CARDS {
            let lower = card.body.to_ascii_lowercase();
            for needle in ["enqueue", "lend", "assignable", "spawn", "pair", "add worker"] {
                assert!(
                    !lower.contains(needle),
                    "{} body must not offer {needle}: {}",
                    card.name,
                    card.body
                );
            }
        }
    }

    #[test]
    fn ack_unlocks_lend_ui_not_assignable() {
        assert!(!disclaimer_ack_enables_lend(false));
        assert!(disclaimer_ack_enables_lend(true));
        for accepted in [false, true] {
            assert!(
                !disclaimer_ack_makes_assignable(accepted),
                "ack={accepted} must not make partners assignable"
            );
        }
    }

    #[test]
    fn cloud_page_live_source_stays_coming_soon_placeholders() {
        // just test excludes fleet-ui; lock every production rsx! block
        // (CloudPage + CloudCard) so a picker control fails on the default unit path.
        let src = include_str!("../../fleet-ui/src/pages/runners/cloud/page.rs");
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("cloud page.rs production source");
        let mut blocks = Vec::new();
        let mut rest = prod;
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
            blocks.push(&after[..end]);
            rest = &after[end..];
        }
        assert!(
            !blocks.is_empty(),
            "production Cloud page must contain rsx! markup"
        );
        let joined = blocks.join("\n").to_ascii_lowercase();
        assert!(
            joined.contains("attach fleet cloud"),
            "Cloud page must expose Attach Fleet Cloud"
        );
        for needle in ["lend", "enqueue", "add worker"] {
            assert!(
                !joined.contains(needle),
                "Cloud rsx! must not contain {needle:?} (assignable risk)"
            );
        }
    }
}
