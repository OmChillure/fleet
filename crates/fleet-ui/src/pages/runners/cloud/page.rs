//! Cloud page: AWS, GCP, Azure, and Fleet Cloud coming-soon cards.

use dioxus::prelude::*;

/// Workspace-scoped Cloud runners placeholder (`/{workspace_id}/runners/cloud`).
#[component]
pub fn CloudPage(workspace_id: String) -> Element {
    let _ = workspace_id;

    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-3xl flex-col overflow-auto px-4 py-6 md:px-6",
            h1 { class: "m-0 text-2xl font-semibold tracking-tight text-[var(--app-fg)]",
                "Cloud"
            }
            p { class: "mt-1 text-sm text-[var(--app-muted)]",
                "Bring your own AWS, GCP, or Azure, or use Fleet Cloud."
            }
            div { class: "mt-6 grid grid-cols-1 gap-3 sm:grid-cols-2",
                CloudCard {
                    name: "AWS",
                    body: "Bring-your-own AWS. Customer keys stay in the workspace; secrets never enter a sandbox.",
                }
                CloudCard {
                    name: "GCP",
                    body: "Bring-your-own GCP. Customer keys stay in the workspace; secrets never enter a sandbox.",
                }
                CloudCard {
                    name: "Azure",
                    body: "Bring-your-own Azure. Customer keys stay in the workspace; secrets never enter a sandbox.",
                }
                CloudCard {
                    name: "Fleet Cloud",
                    body: "Paid pool of Fleet-hosted runners. You do not bring cloud keys.",
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
                    "Coming soon"
                }
            }
            p { class: "mt-2 text-sm text-[var(--app-muted)]",
                "{body}"
            }
        }
    }
}
