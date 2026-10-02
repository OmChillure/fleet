//! Microsoft Teams chats → one ops job. No channel filter on poll.

use dioxus::prelude::*;

use crate::pages::inbox::designer::workflow;
use crate::pages::inbox::experience::{experience, slack_parts};
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
        section { class: "rounded-2xl border {exp.accent} bg-[var(--app-bg-elevated)] p-4",
            h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                "What Fleet lists"
            }
            p { class: "mt-1 text-sm text-[var(--app-muted)]",
                "Refresh lists chats the connected Microsoft 365 account can already see, then pulls recent messages. There is no channel allowlist on this poll — connect the work account that sits in the team."
            }
        }
        {hook_card(data)}
        h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Recent messages" }
        {empty_or_list(data, rsx! {
            ul { class: "m-0 list-none space-y-2 p-0",
                for t in data.rows() {
                    {
                        let (chat, mid) = slack_parts(&t.key);
                        rsx! {
                            li {
                                key: "{t.provider}:{t.key}",
                                class: "flex flex-wrap items-start justify-between gap-3 rounded-xl border {exp.accent} bg-[var(--app-bg-elevated)] px-4 py-3",
                                div { class: "min-w-0 flex-1",
                                    p { class: "m-0 truncate font-mono text-[11px] {exp.mark}",
                                        "{chat}"
                                    }
                                    p { class: "m-0 mt-1 text-sm leading-snug text-[var(--app-fg)]",
                                        "{t.title}"
                                    }
                                    {preview(&t)}
                                    if !mid.is_empty() {
                                        p { class: "m-0 mt-1 font-mono text-[10px] text-[var(--app-muted)]",
                                            "{mid}"
                                        }
                                    }
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
