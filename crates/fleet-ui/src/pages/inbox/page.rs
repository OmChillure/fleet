//! Ticket inbox: monitor connected boards and finish items into a fleet run.

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

use super::api::{
    InboxResponse, MonitorPatch, MonitorState, TicketRow, finish_ticket, list_inbox, poll_inbox,
    start_monitor, stop_monitor, update_monitor,
};
use crate::components::toast::show_toast;

fn status_for(resp: &InboxResponse) -> String {
    let n = resp.tickets.len();
    match resp.monitor.as_ref() {
        Some(m) => {
            let extra = if m.enabled {
                "monitor on"
            } else {
                "monitor off"
            };
            let poll = m
                .last_poll_at
                .as_deref()
                .map(|s| s.get(..19).unwrap_or(s).replace('T', " "))
                .unwrap_or_else(|| "never".into());
            format!("{n} ticket(s) · {extra} · last poll {poll}")
        }
        None => format!("{n} ticket(s)"),
    }
}

#[component]
pub fn Inbox(workspace_id: String) -> Element {
    let mut rows = use_signal(Vec::<TicketRow>::new);
    let mut monitor = use_signal(|| None::<MonitorState>);
    let mut status_line = use_signal(|| "Loading inbox…".to_string());
    let mut busy = use_signal(|| false);
    let mut github_repos = use_signal(String::new);
    let mut slack_channels = use_signal(String::new);
    let mut jira_jql = use_signal(String::new);
    let mut notion_dbs = use_signal(String::new);
    let mut auto_launch = use_signal(|| false);

    let ws_list = workspace_id.clone();
    use_future(move || {
        let ws = ws_list.clone();
        async move {
            loop {
                match list_inbox(&ws).await {
                    Ok(resp) => {
                        if monitor().is_none() {
                            if let Some(m) = resp.monitor.as_ref() {
                                github_repos.set(m.github_repos.join(", "));
                                slack_channels.set(m.slack_channels.join(", "));
                                jira_jql.set(m.jira_jql.clone().unwrap_or_default());
                                notion_dbs.set(m.notion_databases.join(", "));
                                auto_launch.set(m.auto_launch);
                            }
                        }
                        status_line.set(status_for(&resp));
                        monitor.set(resp.monitor.clone());
                        rows.set(resp.tickets);
                    }
                    Err(e) => status_line.set(e.message()),
                }
                TimeoutFuture::new(4000).await;
            }
        }
    });

    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-4xl flex-col gap-4 overflow-auto p-4 md:p-5",
            div { class: "flex flex-wrap items-start justify-between gap-3",
                div {
                    h1 { class: "m-0 text-2xl font-bold tracking-tight text-[var(--app-fg)]", "Inbox" }
                    p { class: "mt-1 text-sm text-[var(--app-muted)]",
                        "Watches every connected integration. Finish starts one generalist on the item — the agent chooses tools from what you asked. Chat finish_ticket only loads the spec so the model can plan."
                    }
                    p { class: "mt-0.5 font-mono text-xs text-[var(--app-muted)]", "{status_line}" }
                    if let Some(err) = monitor().and_then(|m| m.last_error) {
                        p { class: "mt-1 text-xs text-red-400", "{err}" }
                    }
                }
                div { class: "flex flex-wrap gap-2",
                    button {
                        class: "rounded-lg border border-[var(--app-border)] px-3 py-2 text-sm text-[var(--app-fg)] disabled:opacity-50",
                        r#type: "button",
                        disabled: busy(),
                        onclick: {
                            let ws = workspace_id.clone();
                            move |_| {
                                if busy() { return; }
                                busy.set(true);
                                let ws = ws.clone();
                                spawn(async move {
                                    match start_monitor(&ws).await {
                                        Ok(_) => show_toast().success("Monitor on — polling connected boards."),
                                        Err(e) => show_toast().error(e.message()),
                                    }
                                    if let Ok(resp) = list_inbox(&ws).await {
                                        rows.set(resp.tickets.clone());
                                        monitor.set(resp.monitor.clone());
                                        status_line.set(status_for(&resp));
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                        "Start monitor"
                    }
                    button {
                        class: "rounded-lg border border-[var(--app-border)] px-3 py-2 text-sm text-[var(--app-fg)] disabled:opacity-50",
                        r#type: "button",
                        disabled: busy() || !monitor().map(|m| m.enabled).unwrap_or(false),
                        onclick: {
                            let ws = workspace_id.clone();
                            move |_| {
                                if busy() { return; }
                                busy.set(true);
                                let ws = ws.clone();
                                spawn(async move {
                                    match stop_monitor(&ws).await {
                                        Ok(_) => show_toast().success("Monitor off."),
                                        Err(e) => show_toast().error(e.message()),
                                    }
                                    if let Ok(resp) = list_inbox(&ws).await {
                                        rows.set(resp.tickets.clone());
                                        monitor.set(resp.monitor.clone());
                                        status_line.set(status_for(&resp));
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                        "Stop monitor"
                    }
                    button {
                        class: "rounded-lg border border-[var(--app-border)] px-3 py-2 text-sm text-[var(--app-fg)] disabled:opacity-50",
                        r#type: "button",
                        disabled: busy(),
                        onclick: {
                            let ws = workspace_id.clone();
                            move |_| {
                                if busy() { return; }
                                busy.set(true);
                                let ws = ws.clone();
                                spawn(async move {
                                    match poll_inbox(&ws).await {
                                        Ok(_) => show_toast().success("Polled."),
                                        Err(e) => show_toast().error(e.message()),
                                    }
                                    if let Ok(resp) = list_inbox(&ws).await {
                                        rows.set(resp.tickets.clone());
                                        monitor.set(resp.monitor.clone());
                                        status_line.set(status_for(&resp));
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                        "Refresh now"
                    }
                }
            }

            section { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Monitor filters" }
                p { class: "mt-1 text-xs text-[var(--app-muted)]",
                    "Optional. Empty = poll every app you connected under Integrations."
                }
                label { class: "mt-3 block text-[10px] uppercase tracking-wide text-[var(--app-muted)]",
                    "GitHub repos (comma, owner/name)"
                }
                input {
                    class: "mt-1 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                    value: "{github_repos}",
                    oninput: move |e| github_repos.set(e.value()),
                }
                label { class: "mt-3 block text-[10px] uppercase tracking-wide text-[var(--app-muted)]",
                    "Slack channels (comma, names or IDs)"
                }
                input {
                    class: "mt-1 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                    value: "{slack_channels}",
                    oninput: move |e| slack_channels.set(e.value()),
                }
                label { class: "mt-3 block text-[10px] uppercase tracking-wide text-[var(--app-muted)]",
                    "Jira JQL"
                }
                input {
                    class: "mt-1 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                    value: "{jira_jql}",
                    oninput: move |e| jira_jql.set(e.value()),
                }
                label { class: "mt-3 block text-[10px] uppercase tracking-wide text-[var(--app-muted)]",
                    "Notion databases (comma, IDs)"
                }
                input {
                    class: "mt-1 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                    value: "{notion_dbs}",
                    oninput: move |e| notion_dbs.set(e.value()),
                }
                label { class: "mt-3 flex items-center gap-2 text-sm text-[var(--app-fg)]",
                    input {
                        r#type: "checkbox",
                        checked: auto_launch(),
                        onchange: move |e| auto_launch.set(e.checked()),
                    }
                    "Auto-launch new items from any connected source"
                }
                button {
                    class: "mt-3 rounded-lg border border-[var(--app-border)] px-3 py-2 text-sm text-[var(--app-fg)] disabled:opacity-50",
                    r#type: "button",
                    disabled: busy(),
                    onclick: {
                        let ws = workspace_id.clone();
                        move |_| {
                            if busy() { return; }
                            busy.set(true);
                            let ws = ws.clone();
                            let repos: Vec<String> = github_repos()
                                .split(',')
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect();
                            let channels: Vec<String> = slack_channels()
                                .split(',')
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect();
                            let jql = jira_jql();
                            let notion: Vec<String> = notion_dbs()
                                .split(',')
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect();
                            let launch = auto_launch();
                            spawn(async move {
                                match update_monitor(
                                    &ws,
                                    &MonitorPatch {
                                        github_repos: Some(repos),
                                        slack_channels: Some(channels),
                                        jira_jql: Some(jql),
                                        notion_databases: Some(notion),
                                        auto_launch: Some(launch),
                                        ..MonitorPatch::default()
                                    },
                                )
                                .await
                                {
                                    Ok(_) => show_toast().success("Monitor settings saved."),
                                    Err(e) => show_toast().error(e.message()),
                                }
                                busy.set(false);
                            });
                        }
                    },
                    "Save filters"
                }
            }

            if rows().is_empty() {
                p { class: "rounded-2xl border border-dashed border-[var(--app-border)] p-8 text-center text-sm text-[var(--app-muted)]",
                    "No tickets yet. Connect integrations, then Start monitor."
                }
            } else {
                ul { class: "m-0 list-none space-y-2 p-0",
                    for t in rows() {
                        li {
                            key: "{t.provider}:{t.key}",
                            class: "flex flex-wrap items-center justify-between gap-2 rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] px-4 py-3",
                            div { class: "min-w-0",
                                p { class: "m-0 text-sm font-medium text-[var(--app-fg)]",
                                    "{t.key} · {t.title}"
                                }
                                p { class: "m-0 mt-0.5 font-mono text-[11px] text-[var(--app-muted)]",
                                    "{t.provider} · {t.state}"
                                    if let Some(run) = &t.run_id {
                                        " · run {run}"
                                    }
                                }
                            }
                            if t.run_id.is_none() && t.state != "closed" {
                                button {
                                    class: "rounded-lg bg-[var(--app-fg)] px-3 py-1.5 text-sm font-semibold text-[var(--app-bg)] disabled:opacity-50",
                                    r#type: "button",
                                    disabled: busy(),
                                    onclick: {
                                        let ws = workspace_id.clone();
                                        let provider = t.provider.clone();
                                        let key = t.key.clone();
                                        move |_| {
                                            if busy() { return; }
                                            busy.set(true);
                                            let ws = ws.clone();
                                            let provider = provider.clone();
                                            let key = key.clone();
                                            spawn(async move {
                                                match finish_ticket(&ws, &provider, &key).await {
                                                    Ok(_) => show_toast().success("Fleet launched."),
                                                    Err(e) => show_toast().error(e.message()),
                                                }
                                                if let Ok(resp) = list_inbox(&ws).await {
                                                    rows.set(resp.tickets.clone());
                                                    monitor.set(resp.monitor.clone());
                                                    status_line.set(status_for(&resp));
                                                }
                                                busy.set(false);
                                            });
                                        }
                                    },
                                    "Finish"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
