//! Shared split chrome: form (left) + product panel (right).

use dioxus::prelude::*;

use crate::Route;

#[component]
pub fn AuthSplit(form: Element, right_title: String, right_subtitle: String) -> Element {
    rsx! {
        div { class: "grid min-h-[calc(100vh-4rem)] w-full bg-black text-white lg:grid-cols-2",
            // —— Left: form ——
            div { class: "flex flex-col justify-center px-6 py-10 sm:px-10 lg:px-14 xl:px-20",
                div { class: "mb-8 lg:hidden",
                    Link { class: "inline-flex items-center gap-2 font-bold text-white", to: Route::Home {},
                        div { class: "h-6 w-6 rounded-md bg-white" }
                        span { "Agent Fleet" }
                    }
                }
                div { class: "mx-auto w-full max-w-md",
                    {form}
                }
            }

            // —— Right: half-page story panel ——
            div { class: "relative hidden overflow-hidden border-l border-neutral-900 bg-neutral-950 lg:flex lg:flex-col lg:justify-between",
                // subtle grid
                div {
                    class: "pointer-events-none absolute inset-0 opacity-[0.35]",
                    style: "background-image: linear-gradient(to right, #262626 1px, transparent 1px), linear-gradient(to bottom, #262626 1px, transparent 1px); background-size: 48px 48px;",
                }
                div { class: "pointer-events-none absolute -right-24 -top-24 h-72 w-72 rounded-full bg-white/5 blur-3xl" }
                div { class: "pointer-events-none absolute -bottom-32 -left-16 h-80 w-80 rounded-full bg-white/5 blur-3xl" }

                div { class: "relative z-10 flex flex-1 flex-col justify-center px-12 py-14 xl:px-16",
                    span { class: "mb-4 inline-flex w-fit items-center rounded-full border border-neutral-800 bg-black/60 px-3 py-1 text-[0.72rem] font-semibold uppercase tracking-wider text-neutral-400",
                        "Operating system for AI workforces"
                    }
                    h2 { class: "max-w-md text-3xl font-bold tracking-tight text-white xl:text-4xl",
                        "{right_title}"
                    }
                    p { class: "mt-4 max-w-md text-[1.02rem] leading-relaxed text-neutral-400",
                        "{right_subtitle}"
                    }

                    ul { class: "mt-10 space-y-4",
                        AuthFeature {
                            title: "Sandboxed by default".to_string(),
                            body: "Agents start offline. Egress is allowlisted — secrets never enter the sandbox.".to_string(),
                        }
                        AuthFeature {
                            title: "Built for parallel fleets".to_string(),
                            body: "Claim queues and durable sessions so a thousand workers do not deadlock each other.".to_string(),
                        }
                        AuthFeature {
                            title: "Humans at policy edges".to_string(),
                            body: "Approve merges and deploys; let the rest of the fleet run autonomously.".to_string(),
                        }
                    }
                }

                div { class: "relative z-10 border-t border-neutral-900 px-12 py-6 text-sm text-neutral-500 xl:px-16",
                    "Agent Fleet · control plane for sandboxed coding agents"
                }
            }
        }
    }
}

#[component]
fn AuthFeature(title: String, body: String) -> Element {
    rsx! {
        li { class: "flex gap-3",
            div { class: "mt-1 h-2 w-2 shrink-0 rounded-full bg-white" }
            div {
                p { class: "text-sm font-semibold text-white", "{title}" }
                p { class: "mt-0.5 text-sm text-neutral-500", "{body}" }
            }
        }
    }
}
