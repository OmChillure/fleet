//! Linear team issues (ENG-123) → Fleet work + review.

use dioxus::prelude::*;

use crate::pages::inbox::designer::workflow;
use crate::pages::inbox::experience::experience;
use crate::pages::inbox::shared::{
    action_cell, empty_or_list, error_banner, flow, frame, header, hook_card, need_connect,
    open_in, preview, PageData,
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
        h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Issues" }
        {empty_or_list(data, rsx! {
            ul { class: "m-0 list-none space-y-2 p-0",
                for t in data.rows() {
                    { rsx! {
                        li {
                            key: "{t.provider}:{t.key}",
                            class: "flex flex-wrap items-start justify-between gap-3 rounded-xl border {exp.accent} bg-[var(--app-bg-elevated)] px-4 py-3",
                            div { class: "min-w-0 flex-1",
                                p { class: "m-0 text-sm font-medium text-[var(--app-fg)]",
                                    span { class: "mr-2 inline-block rounded-md {exp.wash} px-1.5 py-0.5 font-mono text-[11px] {exp.mark}",
                                        "{t.key}"
                                    }
                                    "{t.title}"
                                }
                                p { class: "m-0 mt-1 text-[11px] text-[var(--app-muted)]",
                                    "{t.state}"
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
