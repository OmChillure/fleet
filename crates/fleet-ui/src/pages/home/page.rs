use dioxus::prelude::*;

use crate::Route;

#[component]
pub fn Home() -> Element {
    rsx! {
        // Pure black landing — no color accents
        section { class: "w-full bg-black pb-16 pt-16 text-white",
            div { class: "w-full px-4",
                span { class: "inline-flex items-center rounded-full border border-neutral-800 bg-neutral-950 px-2.5 py-1 text-[0.78rem] font-semibold uppercase tracking-wide text-neutral-400",
                    "Control plane for agent fleets"
                }
                h1 { class: "mt-4 mb-3.5 max-w-3xl text-[clamp(2rem,4vw,3rem)] leading-tight font-bold tracking-tight text-white",
                    "Delegate work to sandboxed agents — watch them ship"
                }
                p { class: "mb-8 max-w-xl text-[1.05rem] text-neutral-400",
                    "Agent Fleet plans, coordinates, and monitors fleets of coding agents. "
                    "Configure teams, open tasks, and keep humans in the loop when it matters."
                }
                div { class: "flex flex-wrap gap-3",
                    Link {
                        class: "inline-flex items-center justify-center rounded-lg bg-white px-4 py-2.5 text-[0.92rem] font-semibold text-black transition hover:bg-neutral-200",
                        to: Route::Login {},
                        "Log in"
                    }
                    Link {
                        class: "inline-flex items-center justify-center rounded-lg border border-neutral-700 bg-transparent px-4 py-2.5 text-[0.92rem] font-semibold text-white transition hover:border-neutral-500 hover:bg-neutral-950",
                        to: Route::Dashboard {},
                        "View dashboard"
                    }
                }
            }
        }

        section { class: "w-full bg-black pb-16 pt-4 text-white",
            div { class: "grid w-full grid-cols-[repeat(auto-fit,minmax(220px,1fr))] gap-4 px-4",
                div { class: "rounded-lg border border-neutral-800 bg-neutral-950 px-5 py-4",
                    h3 { class: "mb-1.5 text-base font-semibold text-white", "Sandboxed execution" }
                    p { class: "m-0 text-sm text-neutral-400",
                        "Containers or VMs per workflow — isolated worktrees, shared policy."
                    }
                }
                div { class: "rounded-lg border border-neutral-800 bg-neutral-950 px-5 py-4",
                    h3 { class: "mb-1.5 text-base font-semibold text-white", "Human in the loop" }
                    p { class: "m-0 text-sm text-neutral-400",
                        "Approvals, plan review, and PR sign-off before anything merges."
                    }
                }
                div { class: "rounded-lg border border-neutral-800 bg-neutral-950 px-5 py-4",
                    h3 { class: "mb-1.5 text-base font-semibold text-white", "Observability" }
                    p { class: "m-0 text-sm text-neutral-400",
                        "Session streams, sandbox health, and fleet-wide KPIs in one place."
                    }
                }
            }
        }
    }
}
