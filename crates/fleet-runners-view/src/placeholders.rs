//! Cloud / Partners placeholder copy (extracted from fleet-ui).
//!
//! These surfaces cannot enqueue work. Disclaimer ack is a timestamp only:
//! it does not enable lending or make a partner assignable.

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
    body: "Bring-your-own Azure. Customer keys stay in the workspace; secrets never enter a sandbox.",
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

/// Recording an ack does not enable lending or buying credits.
pub fn disclaimer_ack_enables_lend(_accepted: bool) -> bool {
    false
}

/// Recording an ack does not make a partner host assignable.
pub fn disclaimer_ack_makes_assignable(_accepted: bool) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // catalog:unit-cloud-partners-helpers — Cloud/Partners page render
    // helpers: four Coming soon cards, trust copy, disclaimer default-off,
    // ack is not lend/assignable.

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
    fn every_cloud_card_is_coming_soon() {
        assert_eq!(COMING_SOON_BADGE, "Coming soon");
        for card in CLOUD_CARDS {
            assert_eq!(card.badge(), COMING_SOON_BADGE);
            assert_eq!(card.badge(), "Coming soon");
        }
    }

    #[test]
    fn cloud_card_bodies_are_placeholders_not_actions() {
        assert_eq!(
            CLOUD_CARD_AWS.body,
            "Bring-your-own AWS. Customer keys stay in the workspace; secrets never enter a sandbox."
        );
        assert_eq!(
            CLOUD_CARD_GCP.body,
            "Bring-your-own GCP. Customer keys stay in the workspace; secrets never enter a sandbox."
        );
        assert_eq!(
            CLOUD_CARD_AZURE.body,
            "Bring-your-own Azure. Customer keys stay in the workspace; secrets never enter a sandbox."
        );
        assert_eq!(
            CLOUD_CARD_FLEET.body,
            "Paid pool of Fleet-hosted runners. You do not bring cloud keys."
        );
        for card in CLOUD_CARDS {
            let lower = card.body.to_ascii_lowercase();
            assert!(
                !lower.contains("enqueue")
                    && !lower.contains("lend")
                    && !lower.contains("assignable"),
                "{} body must not offer enqueue/lend: {}",
                card.name,
                card.body
            );
        }
    }

    #[test]
    fn partners_coming_soon_and_trust_copy() {
        assert_eq!(COMING_SOON_BADGE, "Coming soon");
        assert_eq!(PARTNERS_TRUST_COPY.len(), 4);
        assert_eq!(
            PARTNERS_TRUST_COPY[0],
            "Work and source leave your trust boundary."
        );
        assert_eq!(
            PARTNERS_TRUST_COPY[1],
            "Fleet is not responsible for what a partner does with it."
        );
        assert!(
            PARTNERS_TRUST_COPY[2].contains("Verified vs open"),
            "{}",
            PARTNERS_TRUST_COPY[2]
        );
        assert!(
            PARTNERS_TRUST_COPY[3].contains("0.9 per 1.0")
                && PARTNERS_TRUST_COPY[3].contains("no cash-out"),
            "{}",
            PARTNERS_TRUST_COPY[3]
        );
    }

    #[test]
    fn disclaimer_defaults_off() {
        assert!(
            !DISCLAIMER_DEFAULT_ACCEPTED,
            "disclaimer must start unaccepted"
        );
        assert!(!disclaimer_ack_enables_lend(DISCLAIMER_DEFAULT_ACCEPTED));
        assert!(!disclaimer_ack_makes_assignable(
            DISCLAIMER_DEFAULT_ACCEPTED
        ));
    }

    #[test]
    fn ack_does_not_enable_lend_or_assignable() {
        for accepted in [false, true] {
            assert!(
                !disclaimer_ack_enables_lend(accepted),
                "ack={accepted} must not enable lend"
            );
            assert!(
                !disclaimer_ack_makes_assignable(accepted),
                "ack={accepted} must not make partners assignable"
            );
        }
    }
}
