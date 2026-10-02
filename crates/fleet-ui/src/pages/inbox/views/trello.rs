//! Trello cards on selected boards → Fleet work + review.

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
        {hook_card(data)}
        h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Cards" }
        {empty_or_list(data, rsx! {
            ul { class: "m-0 grid list-none gap-2 p-0 sm:grid-cols-2",
                for t in data.rows() {
                    { rsx! {
                        li {
                            key: "{t.provider}:{t.key}",
                            class: "flex flex-col justify-between rounded-xl border {exp.accent} bg-[var(--app-bg-elevated)] px-4 py-3",
                            div {
                                p { class: "m-0 text-[10px] uppercase tracking-wide {exp.mark}",
                                    "Card · {t.state}"
                                }
                                p { class: "m-0 mt-1 text-sm font-medium text-[var(--app-fg)]",
                                    "{t.title}"
                                }
                                {preview(&t)}
                                {open_in(&t, name)}
                            }
                            div { class: "mt-3 flex justify-end",
                                {action_cell(ws, &t, data)}
                            }
                        }
                    } }
                }
            }
        })}
    })
}
