//! Cloud page: AWS, GCP, Azure, and Fleet Cloud coming-soon cards.

use dioxus::prelude::*;
use fleet_runners_view::placeholders::{CLOUD_CARDS, COMING_SOON_BADGE, CloudCardCopy};
use fleet_runners_view::routes::NAV_LABEL_CLOUD;

/// One Cloud card as this page renders it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CloudPageCard {
    name: &'static str,
    body: &'static str,
    badge: &'static str,
}

/// Interactive control the Cloud page might expose (spawn / lend / pair).
/// The live page must keep this list empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CloudPageControl {
    label: &'static str,
    enabled: bool,
}

/// Render model for `/{workspace_id}/runners/cloud`.
/// Workspace id does not change the surface: placeholders only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CloudPageModel {
    heading: &'static str,
    cards: [CloudPageCard; 4],
    controls: &'static [CloudPageControl],
}

const CLOUD_PAGE_CONTROLS: &[CloudPageControl] = &[];

const CLOUD_PLACEHOLDER_NAMES: [&str; 4] = ["AWS", "GCP", "Azure", "Fleet Cloud"];

fn cloud_page_card(copy: CloudCardCopy) -> CloudPageCard {
    CloudPageCard {
        name: copy.name,
        body: copy.body,
        badge: COMING_SOON_BADGE,
    }
}

/// Page surface is independent of workspace: four Coming soon cards, no actions.
fn cloud_page_model(_workspace_id: &str) -> CloudPageModel {
    CloudPageModel {
        heading: NAV_LABEL_CLOUD,
        cards: [
            cloud_page_card(CLOUD_CARDS[0]),
            cloud_page_card(CLOUD_CARDS[1]),
            cloud_page_card(CLOUD_CARDS[2]),
            cloud_page_card(CLOUD_CARDS[3]),
        ],
        controls: CLOUD_PAGE_CONTROLS,
    }
}

/// Names the picker treats as public-cloud / Fleet Cloud placeholders.
/// Anything else on this page is an assignable-risk (silent new kind).
fn cloud_card_kind_is_assignable(name: &str) -> bool {
    !CLOUD_PLACEHOLDER_NAMES.contains(&name)
}

fn control_is_spawn_or_enqueue(label: &str) -> bool {
    let l = label.to_ascii_lowercase();
    l.contains("spawn")
        || l.contains("enqueue")
        || l.contains("add worker")
        || l.contains("pair")
        || l.contains("assign")
        || l.contains("lease")
}

fn control_is_lend(label: &str) -> bool {
    let l = label.to_ascii_lowercase();
    l.contains("lend") || l.contains("buy credit") || l.contains("offer capacity")
}

impl CloudPageModel {
    fn has_spawn_or_lend_control(self) -> bool {
        self.controls
            .iter()
            .any(|c| control_is_spawn_or_enqueue(c.label) || control_is_lend(c.label))
    }

    fn can_enqueue(self) -> bool {
        self.controls
            .iter()
            .any(|c| c.enabled && control_is_spawn_or_enqueue(c.label))
    }

    fn can_lend(self) -> bool {
        self.controls
            .iter()
            .any(|c| c.enabled && control_is_lend(c.label))
    }

    fn can_spawn(self) -> bool {
        self.can_enqueue()
    }

    /// Cloud placeholders cannot become assignable: Coming soon, known names,
    /// no enabled spawn/lend.
    fn is_assignable(self) -> bool {
        if self.can_enqueue() || self.can_lend() {
            return true;
        }
        self.cards
            .iter()
            .any(|c| c.badge != COMING_SOON_BADGE || cloud_card_kind_is_assignable(c.name))
    }
}

/// Workspace-scoped Cloud runners placeholder (`/{workspace_id}/runners/cloud`).
#[component]
pub fn CloudPage(workspace_id: String) -> Element {
    let model = cloud_page_model(&workspace_id);

    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-3xl flex-col overflow-auto px-4 py-6 md:px-6",
            h1 { class: "m-0 text-2xl font-semibold tracking-tight text-[var(--app-fg)]",
                "{model.heading}"
            }
            p { class: "mt-1 text-sm text-[var(--app-muted)]",
                "Bring your own AWS, GCP, or Azure, or use Fleet Cloud."
            }
            div { class: "mt-6 grid grid-cols-1 gap-3 sm:grid-cols-2",
                for card in model.cards {
                    CloudCard {
                        name: card.name,
                        body: card.body,
                        badge: card.badge,
                    }
                }
            }
        }
    }
}

#[component]
fn CloudCard(name: &'static str, body: &'static str, badge: &'static str) -> Element {
    rsx! {
        article { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
            div { class: "flex flex-wrap items-center gap-2",
                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                    "{name}"
                }
                span { class: "inline-flex items-center rounded-full border border-[var(--app-border)] bg-[var(--app-bg-hover)] px-2 py-0.5 text-xs font-medium text-[var(--app-muted)]",
                    "{badge}"
                }
            }
            p { class: "mt-2 text-sm text-[var(--app-muted)]",
                "{body}"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // catalog:unit-cloud-page — four Coming soon cards; no spawn/lend;
    // placeholders cannot silently become assignable.

    fn sample_workspaces() -> [&'static str; 4] {
        [
            "",
            "7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90",
            "aws",
            "c0ffee00-0000-4000-8000-00000000cloud",
        ]
    }

    #[test]
    fn cloud_page_is_four_coming_soon_cards() {
        let model = cloud_page_model("7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90");
        assert_eq!(model.heading, "Cloud");
        assert_eq!(model.heading, NAV_LABEL_CLOUD);
        assert_eq!(model.cards.len(), 4);
        let names: Vec<&str> = model.cards.iter().map(|c| c.name).collect();
        assert_eq!(names, CLOUD_PLACEHOLDER_NAMES);
        assert_eq!(names, ["AWS", "GCP", "Azure", "Fleet Cloud"]);
        assert_eq!(COMING_SOON_BADGE, "Coming soon");
        for card in model.cards {
            assert_eq!(card.badge, COMING_SOON_BADGE);
            assert_eq!(card.badge, "Coming soon");
            assert!(!card.name.is_empty(), "cloud card missing name");
            assert!(
                !card.body.is_empty(),
                "{} body must not be empty",
                card.name
            );
        }
    }

    #[test]
    fn cloud_page_has_no_spawn_or_lend_controls() {
        let model = cloud_page_model("7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90");
        assert!(
            model.controls.is_empty(),
            "cloud page must not expose spawn/lend/pair controls: {:?}",
            model.controls
        );
        assert!(!model.has_spawn_or_lend_control());
        assert!(!model.can_enqueue());
        assert!(!model.can_spawn());
        assert!(!model.can_lend());
    }

    #[test]
    fn cloud_page_cards_are_not_assignable() {
        let model = cloud_page_model("7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90");
        assert!(
            !model.is_assignable(),
            "cloud placeholders must not be assignable"
        );
        for card in model.cards {
            assert!(
                !cloud_card_kind_is_assignable(card.name),
                "{} must stay a non-assignable placeholder",
                card.name
            );
            let lower = card.body.to_ascii_lowercase();
            assert!(
                !lower.contains("enqueue")
                    && !lower.contains("spawn")
                    && !lower.contains("lend")
                    && !lower.contains("assignable"),
                "{} body must not offer enqueue/lend/spawn: {}",
                card.name,
                card.body
            );
        }
    }

    #[test]
    fn workspace_id_does_not_enable_cloud_enqueue() {
        let baseline = cloud_page_model("7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90");
        for ws in sample_workspaces() {
            let model = cloud_page_model(ws);
            assert_eq!(
                model, baseline,
                "workspace {ws:?} must not change the Cloud placeholder surface"
            );
            assert!(!model.can_enqueue());
            assert!(!model.can_lend());
            assert!(!model.can_spawn());
            assert!(!model.is_assignable());
            assert!(!model.has_spawn_or_lend_control());
        }
    }

    #[test]
    fn enabled_spawn_control_makes_cloud_page_assignable() {
        let model = cloud_page_model("7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90");
        let broken = CloudPageModel {
            controls: &[CloudPageControl {
                label: "Spawn",
                enabled: true,
            }],
            ..model
        };
        assert!(broken.has_spawn_or_lend_control());
        assert!(broken.can_spawn());
        assert!(broken.can_enqueue());
        assert!(
            broken.is_assignable(),
            "an enabled Spawn control must fail the not-assignable contract"
        );
    }

    #[test]
    fn enabled_lend_control_makes_cloud_page_assignable() {
        let model = cloud_page_model("7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90");
        let broken = CloudPageModel {
            controls: &[CloudPageControl {
                label: "Lend capacity",
                enabled: true,
            }],
            ..model
        };
        assert!(broken.has_spawn_or_lend_control());
        assert!(broken.can_lend());
        assert!(
            !broken.can_enqueue(),
            "lend is not the same as enqueue/spawn"
        );
        assert!(
            broken.is_assignable(),
            "an enabled Lend control must fail the not-assignable contract"
        );
    }

    #[test]
    fn disabled_spawn_control_still_counts_as_spawn_surface() {
        let model = cloud_page_model("7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90");
        let disabled = CloudPageModel {
            controls: &[CloudPageControl {
                label: "Add worker",
                enabled: false,
            }],
            ..model
        };
        assert!(
            disabled.has_spawn_or_lend_control(),
            "cloud must not grow a spawn/pair control, even disabled"
        );
        assert!(
            !disabled.can_enqueue(),
            "disabled Add worker must not enqueue"
        );
        assert!(
            !disabled.is_assignable(),
            "disabled pair control is not assignable, but it is still a spawn surface"
        );
    }

    #[test]
    fn dropping_coming_soon_badge_is_assignable_risk() {
        let model = cloud_page_model("7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90");
        let mut cards = model.cards;
        cards[0].badge = "Ready";
        let broken = CloudPageModel { cards, ..model };
        assert!(
            broken.is_assignable(),
            "a Cloud card that is no longer Coming soon must not stay non-assignable"
        );
        cards[0].badge = COMING_SOON_BADGE;
        cards[3].badge = "";
        let fleet = CloudPageModel { cards, ..model };
        assert!(
            fleet.is_assignable(),
            "Fleet Cloud without Coming soon is an assignable risk"
        );
    }

    #[test]
    fn unknown_cloud_card_name_is_assignable_risk() {
        assert!(cloud_card_kind_is_assignable("Lambda"));
        assert!(cloud_card_kind_is_assignable("Self-hosted"));
        assert!(cloud_card_kind_is_assignable(""));
        for name in CLOUD_PLACEHOLDER_NAMES {
            assert!(
                !cloud_card_kind_is_assignable(name),
                "{name} is a known placeholder"
            );
        }

        let model = cloud_page_model("7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90");
        let mut cards = model.cards;
        cards[1].name = "GKE ready pool";
        let broken = CloudPageModel { cards, ..model };
        assert!(
            broken.is_assignable(),
            "a new cloud kind must not silently become assignable"
        );
    }

    fn production_src() -> &'static str {
        include_str!("page.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("cloud page source")
    }

    fn live_component_src(prod: &str) -> &str {
        let idx = prod.find("#[component]").expect("CloudPage live component");
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

    fn assert_no_action_surface(chunk: &str, label: &str) {
        let lower = chunk.to_ascii_lowercase();
        assert!(!lower.contains("button"), "{label} must render no buttons");
        // A provider picker that creates hosts is the assignable hole this
        // card locks: cards stay articles, not click-to-POST / select-to-spawn.
        for needle in [
            "onclick", "onsubmit", "href", "select {", "form {", "input {", "combobox",
        ] {
            assert!(
                !lower.contains(needle),
                "{label} must not contain {needle:?} (picker must not create cloud hosts)"
            );
        }
        for needle in ["spawn", "lend", "enqueue", "pair", "add worker"] {
            assert!(
                !lower.contains(needle),
                "{label} must not contain {needle:?} (assignable risk)"
            );
        }
    }

    #[test]
    fn cloud_page_source_stays_coming_soon_placeholders() {
        let prod = production_src();
        assert!(
            prod.contains("CLOUD_CARDS"),
            "live Cloud page must stay wired to CLOUD_CARDS"
        );
        assert!(
            prod.contains("COMING_SOON_BADGE"),
            "live Cloud page must stay wired to COMING_SOON_BADGE"
        );
        assert!(
            prod.contains("cloud_page_card(CLOUD_CARDS[0])")
                && prod.contains("cloud_page_card(CLOUD_CARDS[1])")
                && prod.contains("cloud_page_card(CLOUD_CARDS[2])")
                && prod.contains("cloud_page_card(CLOUD_CARDS[3])"),
            "CloudPage model must render all four CLOUD_CARDS"
        );
        assert!(
            prod.contains("badge: COMING_SOON_BADGE"),
            "each Cloud card must take its badge from COMING_SOON_BADGE"
        );
        assert!(
            prod.contains("let model = cloud_page_model(&workspace_id)"),
            "CloudPage must render through cloud_page_model"
        );
        assert!(
            prod.contains("for card in model.cards"),
            "CloudPage must render model.cards, not a parallel hardcoded list"
        );

        let live = live_component_src(prod);
        assert!(
            !live.contains("model.controls") && !live.contains("CLOUD_PAGE_CONTROLS"),
            "CloudPage must not render the unused controls slice"
        );
        assert_no_action_surface(live, "CloudPage live component");

        let blocks = rsx_blocks(prod);
        assert!(
            !blocks.is_empty(),
            "production Cloud page must contain rsx! markup"
        );
        for block in &blocks {
            assert_no_action_surface(block, "CloudPage rsx!");
        }
    }

    #[test]
    fn cloud_page_is_not_a_host_creating_picker() {
        // catalog:list-cloud-four-provider-cards — AWS/GCP/Azure/Fleet Cloud
        // are Coming soon articles. The page is not a provider picker that
        // POSTs worker-hosts. Clicking a card must not create a cloud host.
        let model = cloud_page_model("7f3a1c2e-9b04-4d55-8e11-0a6b2c4d8e90");
        assert_eq!(
            model.cards.iter().map(|c| c.name).collect::<Vec<_>>(),
            ["AWS", "GCP", "Azure", "Fleet Cloud"]
        );
        assert!(
            model.controls.is_empty(),
            "cloud picker must expose no enqueue/spawn/lend controls: {:?}",
            model.controls
        );
        assert!(!model.can_enqueue());
        assert!(!model.can_spawn());
        assert!(!model.can_lend());
        assert!(!model.is_assignable());
        for card in model.cards {
            assert_eq!(card.badge, "Coming soon");
            assert!(
                !cloud_card_kind_is_assignable(card.name),
                "{} must stay a placeholder, not a create-host picker option",
                card.name
            );
        }

        let prod = production_src();
        let live = live_component_src(prod);
        assert!(
            live.contains("article {"),
            "Cloud cards must render as articles, not a host-creating picker"
        );
        assert!(
            !live.contains("a {") && !live.contains("Link {"),
            "Cloud cards must not be links that create or enqueue a host"
        );
        assert_no_action_surface(live, "CloudPage picker surface");
    }
}
