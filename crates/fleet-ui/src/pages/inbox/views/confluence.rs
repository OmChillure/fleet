//! Confluence wiki pages → analysis runs. No space filter on poll.

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
        {setup(data, "How to set up Confluence")}
        section { class: "rounded-2xl border {exp.accent} bg-[var(--app-bg-elevated)] p-4",
            h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                "What Fleet lists"
            }
            p { class: "mt-1 text-sm text-[var(--app-muted)]",
                "Refresh searches Confluence for pages the token can view. There is no space-key filter on this poll — grant the connection only the spaces you want analyzed."
            }
            p { class: "mt-2 text-xs text-[var(--app-muted)]",
                "Analyze is a read-through of the wiki page. The page is not rewritten unless you ask in Chat."
            }
        }
        {hook_card(data)}
        h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Pages" }
        {empty_or_list(data, rsx! {
            ul { class: "m-0 list-none space-y-2 p-0",
                for t in data.rows() {
                    { rsx! {
                        li {
                            key: "{t.provider}:{t.key}",
                            class: "flex flex-wrap items-start justify-between gap-3 rounded-xl border {exp.accent} bg-[var(--app-bg-elevated)] px-4 py-3",
                            div { class: "min-w-0 flex-1",
                                p { class: "m-0 text-[10px] uppercase tracking-wide {exp.mark}",
                                    "Wiki page"
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
