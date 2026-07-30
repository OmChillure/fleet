use dioxus::prelude::*;

use crate::Route;

#[component]
pub fn Navbar() -> Element {
    rsx! {
        header { class: "sticky top-0 z-50 h-16 border-b border-neutral-800 bg-black/90 backdrop-blur-md",
            div { class: "flex h-full w-full items-center justify-between gap-4 px-4",
                Link { class: "flex items-center gap-2.5 font-bold tracking-tight text-white", to: Route::Home {},
                    div { class: "h-7 w-7 rounded-lg bg-white" }
                    span { class: "text-[1.05rem]", "Agent Fleet" }
                }

                nav { class: "flex items-center gap-1",
                    Link {
                        class: "rounded-lg px-3 py-1.5 text-[0.92rem] font-medium text-neutral-400 transition hover:bg-neutral-900 hover:text-white",
                        to: Route::Home {},
                        active_class: "bg-neutral-900 text-white",
                        "Home"
                    }
                    Link {
                        class: "rounded-lg px-3 py-1.5 text-[0.92rem] font-medium text-neutral-400 transition hover:bg-neutral-900 hover:text-white",
                        to: Route::Dashboard {},
                        active_class: "bg-neutral-900 text-white",
                        "Dashboard"
                    }
                }

                div { class: "flex items-center gap-2",
                    Link {
                        class: "inline-flex items-center justify-center rounded-lg border border-neutral-700 bg-transparent px-4 py-2 text-[0.92rem] font-semibold text-white transition hover:border-neutral-500 hover:bg-neutral-950",
                        to: Route::Login {},
                        "Log in"
                    }
                    Link {
                        class: "inline-flex items-center justify-center rounded-lg bg-white px-4 py-2 text-[0.92rem] font-semibold text-black transition hover:bg-neutral-200",
                        to: Route::Signup {},
                        "Sign up"
                    }
                }
            }
        }
    }
}
