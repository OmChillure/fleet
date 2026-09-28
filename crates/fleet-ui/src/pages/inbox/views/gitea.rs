//! Gitea issues → Fleet run → pull request.

use dioxus::prelude::*;

use crate::pages::inbox::experience::{experience, repo_issue_parts};
use crate::pages::inbox::shared::{
    PageData, action_cell, empty_or_list, error_banner, filter_box, flow, frame, header, hook_card,
    need_connect, open_in, preview, setup,
};

pub fn view(ws: &str, data: PageData) -> Element {
    let exp = experience(data.info);
    let name = data.info.name;
    frame(rsx! {
        {header(ws, data, rsx! { {flow(data)} })}
        {need_connect(ws, data)}
        {error_banner(data)}
        {setup(data, "How to set up Gitea")}
        {filter_box(
            ws,
            data,
            "Repositories as owner/name. Empty means every repo the token can list.",
            false,
        )}
        {hook_card(data)}
        h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Opened issues" }
        {empty_or_list(data, rsx! {
            ul { class: "m-0 list-none divide-y divide-white/5 overflow-hidden rounded-xl border {exp.accent} p-0",
                for t in data.rows() {
                    {
                        let (project, iid) = repo_issue_parts(&t.key);
                        rsx! {
                            li {
                                key: "{t.provider}:{t.key}",
                                class: "flex flex-wrap items-start justify-between gap-3 bg-[var(--app-bg-elevated)] px-4 py-3",
                                div { class: "min-w-0 flex-1",
                                    p { class: "m-0 text-[11px] font-medium uppercase tracking-wide {exp.mark}",
                                        "{project}"
                                    }
                                    p { class: "m-0 mt-1 text-sm font-medium text-[var(--app-fg)]",
                                        span { class: "mr-2 font-mono {exp.mark}", "{iid}" }
                                        "{t.title}"
                                    }
                                    p { class: "m-0 mt-0.5 text-[11px] text-[var(--app-muted)]",
                                        "{t.state} · pull request after Start run"
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
