//! Slack channel messages → one ops job. Invite the bot first.

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
        section { class: "rounded-2xl border {exp.accent} {exp.wash} p-4",
            h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                "Invite the bot into the channel"
            }
            p { class: "mt-1 text-sm text-[var(--app-fg)]",
                "Slack will not let Fleet read or post until the app is a member. Open the channel and type:"
            }
            p { class: "mt-2 rounded-lg bg-black/30 px-3 py-2 font-mono text-sm text-[var(--app-fg)]",
                "/invite @your-app-name"
            }
            p { class: "mt-2 text-xs text-[var(--app-muted)]",
                "Use the name you gave the Slack app. Handle is one ops task, not a coding run. Fleet replies in the channel and skips its own “Fleet run …” messages on the next poll."
            }
        }
        {hook_card(data)}
        h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Recent messages" }
        {empty_or_list(data, rsx! {
            ul { class: "m-0 list-none space-y-2 p-0",
                for t in data.rows() {
                    {
                        let (ch, ts) = slack_parts(&t.key);
                        rsx! {
                            li {
                                key: "{t.provider}:{t.key}",
                                class: "flex flex-wrap items-start justify-between gap-3 rounded-xl border {exp.accent} bg-[var(--app-bg-elevated)] px-4 py-3",
                                div { class: "min-w-0 flex-1",
                                    p { class: "m-0 font-mono text-[11px] {exp.mark}",
                                        "#{ch}"
                                    }
                                    p { class: "m-0 mt-1 text-sm leading-snug text-[var(--app-fg)]",
                                        "{t.title}"
                                    }
                                    {preview(&t)}
                                    if !ts.is_empty() {
                                        p { class: "m-0 mt-1 font-mono text-[10px] text-[var(--app-muted)]",
                                            "{ts}"
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
