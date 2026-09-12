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
