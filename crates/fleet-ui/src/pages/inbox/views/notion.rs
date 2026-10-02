//! Shared Notion databases → analysis runs.

use dioxus::prelude::*;

use crate::pages::inbox::designer::workflow;
use crate::pages::inbox::experience::experience;
use crate::pages::inbox::shared::{
    PageData, action_cell, empty_or_list, error_banner, flow, frame, header, hook_card,
    need_connect, open_in, preview,
};

pub fn view(ws: &str, data: PageData) -> Element {
    let exp = experience(data.info);
    let name = data.info.name;
    frame(rsx! {
        {header(ws, data, rsx! { {flow(data)} })}
        {need_connect(ws, data)}
        {workflow(ws, data)}
        {error_banner(data)}
        section { class: "rounded-2xl border {exp.accent} {exp.wash} p-4",
            h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                "Share the database with Agent Fleet"
            }
            p { class: "mt-1 text-sm text-[var(--app-fg)]",
                "In Notion, open the database → ••• → Add connections → Agent Fleet. Same gesture as sharing a page with a person. Until you do that, Refresh returns nothing."
            }
            p { class: "mt-2 text-xs text-[var(--app-muted)]",
                "Analyze is a read-through, not a coding run. The page stays yours."
            }
        }
        {hook_card(data)}
        h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Shared pages" }
        {empty_or_list(data, rsx! {
            ul { class: "m-0 list-none divide-y divide-white/10 overflow-hidden rounded-xl border {exp.accent} p-0",
                for t in data.rows() {
                    { rsx! {
                        li {
                            key: "{t.provider}:{t.key}",
                            class: "flex flex-wrap items-start justify-between gap-3 bg-[var(--app-bg-elevated)] px-4 py-3",
                            div { class: "min-w-0 flex-1",
                                p { class: "m-0 text-[10px] uppercase tracking-wide text-[var(--app-muted)]",
                                    "Page"
                                }
                                p { class: "m-0 mt-1 text-sm font-medium text-[var(--app-fg)]",
                                    "{t.title}"
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
