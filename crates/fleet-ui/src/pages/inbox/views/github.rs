//! GitHub issues → Fleet run → pull request.

use dioxus::prelude::*;

use crate::pages::inbox::designer::workflow;
use crate::pages::inbox::experience::{experience, repo_issue_parts};
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
        h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Open issues" }
        {empty_or_list(data, rsx! {
            ul { class: "m-0 list-none space-y-2 p-0",
                for t in data.rows() {
                    {
                        let (repo, num) = repo_issue_parts(&t.key);
                        rsx! {
                            li {
                                key: "{t.provider}:{t.key}",
                                class: "flex flex-wrap items-start justify-between gap-3 rounded-xl border {exp.accent} bg-[var(--app-bg-elevated)] px-4 py-3",
                                div { class: "min-w-0 flex-1",
                                    p { class: "m-0 flex flex-wrap items-baseline gap-2 text-sm font-medium text-[var(--app-fg)]",
                                        span { class: "inline-block h-2 w-2 rounded-full bg-emerald-500", "aria-hidden": "true" }
                                        span { class: "font-mono text-[var(--app-muted)]", "{num}" }
                                        span { "{t.title}" }
                                    }
                                    p { class: "m-0 mt-1 font-mono text-[11px] text-[var(--app-muted)]",
                                        "{repo} · {t.state}"
                                    }
                                    {preview(&t)}
                                    {open_in(&t, name)}
                                }
                                {action_cell(ws, &t, data)}
                            }
                        }
                    }
                }
            }
        })}
    })
}
