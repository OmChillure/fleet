//! Cloud page: AWS, GCP, Azure, and Fleet Cloud coming-soon cards.

use dioxus::prelude::*;
use fleet_runners_view::placeholders::{CLOUD_CARDS, COMING_SOON_BADGE};
use fleet_runners_view::routes::NAV_LABEL_CLOUD;

/// Workspace-scoped Cloud runners placeholder (`/{workspace_id}/runners/cloud`).
#[component]
pub fn CloudPage(workspace_id: String) -> Element {
    let _ = workspace_id;

    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-3xl flex-col overflow-auto px-4 py-6 md:px-6",
            h1 { class: "m-0 text-2xl font-semibold tracking-tight text-[var(--app-fg)]",
                "{NAV_LABEL_CLOUD}"
            }
            p { class: "mt-1 text-sm text-[var(--app-muted)]",
                "Bring your own AWS, GCP, or Azure, or use Fleet Cloud."
            }
            div { class: "mt-6 grid grid-cols-1 gap-3 sm:grid-cols-2",
                for card in CLOUD_CARDS {
                    CloudCard {
                        name: card.name,
                        body: card.body,
                    }
                }
            }
        }
    }
}

#[component]
fn CloudCard(name: &'static str, body: &'static str) -> Element {
    rsx! {
        article { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
            div { class: "flex flex-wrap items-center gap-2",
                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                    "{name}"
                }
                span { class: "inline-flex items-center rounded-full border border-[var(--app-border)] bg-[var(--app-bg-hover)] px-2 py-0.5 text-xs font-medium text-[var(--app-muted)]",
                    "{COMING_SOON_BADGE}"
                }
            }
            p { class: "mt-2 text-sm text-[var(--app-muted)]",
                "{body}"
            }
        }
    }
}
