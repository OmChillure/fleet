//! Asana tasks → Fleet work + review. Poll lists incomplete tasks; no project filter.

use dioxus::prelude::*;

use crate::pages::inbox::experience::experience;
use crate::pages::inbox::shared::{
    PageData, action_cell, empty_or_list, error_banner, flow, frame, header, hook_card,
    need_connect, open_in, preview, setup,
};

pub fn view(ws: &str, data: PageData) -> Element {
    let exp = experience(data.info);
    let name = data.info.name;
    frame(rsx! {
        {header(ws, data, rsx! { {flow(data)} })}
        {need_connect(ws, data)}
        {error_banner(data)}
        {setup(data, "How to set up Asana")}
        section { class: "rounded-2xl border {exp.accent} bg-[var(--app-bg-elevated)] p-4",
            h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                "What Fleet lists"
            }
            p { class: "mt-1 text-sm text-[var(--app-muted)]",
                "Refresh asks Asana for incomplete tasks in the first workspace the token can see. There is no project picker on this poll — connect the Asana workspace that already holds the work you want Fleet to take."
            }
        }
        {hook_card(data)}
        h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Open tasks" }
        {empty_or_list(data, rsx! {
            ul { class: "m-0 list-none space-y-2 p-0",
                for t in data.rows() {
                    { rsx! {
                        li {
                            key: "{t.provider}:{t.key}",
                            class: "flex flex-wrap items-start justify-between gap-3 rounded-xl border {exp.accent} bg-[var(--app-bg-elevated)] px-4 py-3",
                            div { class: "min-w-0 flex-1",
                                p { class: "m-0 flex items-start gap-2 text-sm font-medium text-[var(--app-fg)]",
                                    span { class: "mt-0.5 inline-block h-3.5 w-3.5 shrink-0 rounded-full border-2 border-[#F06A6A]" }
                                    span { "{t.title}" }
                                }
                                p { class: "m-0 mt-1 text-[11px] text-[var(--app-muted)]",
                                    "Task · {t.state}"
                                }
                                {preview(&t)}
                                {open_in(&t, name)}
                            }
                            {action_cell(ws, &t, data)}
                        }
                    } }
                }
            }
        })}
    })
}
