//! Bitbucket issues → Fleet run → pull request. No repo filter on poll.

use dioxus::prelude::*;

use crate::pages::inbox::designer::workflow;
use crate::pages::inbox::experience::{experience, repo_issue_parts};
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
        section { class: "rounded-2xl border {exp.accent} bg-[var(--app-bg-elevated)] p-4",
            h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                "What Fleet lists"
            }
            p { class: "mt-1 text-sm text-[var(--app-muted)]",
                "Refresh walks every Bitbucket repo the connected account can see and pulls open issues. There is no repo allowlist on this poll — grant the connection only the workspaces you want Fleet to read."
            }
        }
        {hook_card(data)}
        h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Issues" }
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
                                    p { class: "m-0 font-mono text-[11px] {exp.mark}",
                                        "{repo}{num}"
                                    }
                                    p { class: "m-0 mt-1 text-sm font-medium text-[var(--app-fg)]",
                                        "{t.title}"
                                    }
                                    p { class: "m-0 mt-0.5 text-[11px] text-[var(--app-muted)]",
                                        "{t.state}"
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
