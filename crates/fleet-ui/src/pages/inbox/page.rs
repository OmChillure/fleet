//! Inbox index: work-tools hub, grouped the way a team already uses the apps.

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

use super::api::{InboxResponse, list_inbox, poll_inbox, start_monitor, stop_monitor};
use super::catalog::{INBOX_APPS, InboxGroup};
use crate::Route;
use crate::components::toast::show_toast;
use crate::pages::integrations::{IntegrationLogo, list_integrations};

fn ticket_count(resp: &InboxResponse, provider: &str) -> usize {
    resp.tickets
        .iter()
        .filter(|t| t.provider.eq_ignore_ascii_case(provider))
        .count()
}

#[component]
pub fn Inbox(workspace_id: String) -> Element {
    let mut status_line = use_signal(|| "Loading inbox…".to_string());
    let mut counts = use_signal(std::collections::BTreeMap::<String, usize>::new);
    let mut connected = use_signal(std::collections::BTreeSet::<String>::new);
    let mut monitor_on = use_signal(|| false);
    let mut busy = use_signal(|| false);

    let ws_list = workspace_id.clone();
    use_future(move || {
        let ws = ws_list.clone();
        async move {
            loop {
                let mut next_counts = std::collections::BTreeMap::new();
                let mut next_conn = std::collections::BTreeSet::new();
                match list_inbox(&ws).await {
                    Ok(resp) => {
                        monitor_on.set(resp.monitor.as_ref().is_some_and(|m| m.enabled));
                        for app in INBOX_APPS {
                            next_counts.insert(app.id.to_string(), ticket_count(&resp, app.id));
                        }
                        let n: usize = next_counts.values().sum();
                        let extra = if monitor_on() {
                            "monitor on"
                        } else {
                            "monitor off"
                        };
                        status_line.set(format!("{n} item(s) · {extra}"));
                    }
                    Err(e) => status_line.set(e.message()),
                }
                if let Ok(list) = list_integrations(&ws).await {
                    for i in list {
                        if i.connected {
                            next_conn.insert(i.id.to_ascii_lowercase());
                        }
                    }
                }
                counts.set(next_counts);
                connected.set(next_conn);
                TimeoutFuture::new(8000).await;
            }
        }
    });

    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-4xl flex-col gap-5 overflow-auto p-4 md:p-5",
            div { class: "flex flex-wrap items-start justify-between gap-3",
                div {
                    h1 { class: "m-0 text-2xl font-bold tracking-tight text-[var(--app-fg)]", "Inbox" }
                    p { class: "mt-1 max-w-xl text-sm text-[var(--app-muted)]",
                        "Plug Agent Fleet into the apps your team already uses. Each page walks the connect, what Fleet watches, and what a run does in that product — issues become PRs, Slack messages become ops jobs, Notion pages become analysis."
                    }
                    p { class: "mt-0.5 text-xs text-[var(--app-muted)]", "{status_line}" }
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
                                        Ok(_) => show_toast().success("Monitor on."),
                                        Err(e) => show_toast().error(e.message()),
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
                        disabled: busy(),
                        onclick: {
                            let ws = workspace_id.clone();
                            move |_| {
                                if busy() { return; }
                                busy.set(true);
                                let ws = ws.clone();
                                spawn(async move {
                                    match poll_inbox(&ws).await {
                                        Ok(_) => show_toast().success("Checked connected apps."),
                                        Err(e) => show_toast().error(e.message()),
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                        "Refresh now"
                    }
                    button {
                        class: "rounded-lg border border-[var(--app-border)] px-3 py-2 text-sm text-[var(--app-fg)] disabled:opacity-50",
                        r#type: "button",
                        disabled: busy() || !monitor_on(),
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
                                    busy.set(false);
                                });
                            }
                        },
                        "Stop monitor"
                    }
                }
            }
            p { class: "m-0 text-xs text-[var(--app-muted)]",
                "Start monitor checks connected apps on a schedule. Refresh now checks once. Open an app to connect it and finish setup there — Slack is channels, GitHub is issues."
            }

            for group in InboxGroup::ALL {
                {
                    let mut apps: Vec<_> = INBOX_APPS.iter().filter(|a| a.group == group).collect();
                    apps.sort_by_key(|a| (!connected().contains(a.id), a.name.to_ascii_lowercase()));
                    rsx! {
                        section { key: "{group.title()}",
                            h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                                "{group.title()}"
                            }
                            p { class: "mt-1 mb-3 text-xs text-[var(--app-muted)]",
                                "{group.subtitle()}"
                            }
                            ul { class: "m-0 grid list-none gap-3 p-0 sm:grid-cols-2",
                                for app in apps {
                                    {
                                        let on = connected().contains(app.id);
                                        let n = *counts().get(app.id).unwrap_or(&0);
                                        rsx! {
                                            li { key: "{app.id}",
                                                Link {
                                                    to: Route::InboxApp {
                                                        workspace_id: workspace_id.clone(),
                                                        provider: app.id.to_string(),
                                                    },
                                                    class: "flex h-full items-start gap-3 rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4 text-inherit no-underline hover:bg-[var(--app-bg-hover)]",
                                                    IntegrationLogo { icon: app.id.to_string(), name: app.name.to_string() }
                                                    div { class: "min-w-0",
                                                        p { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                                                            "{app.name}"
                                                        }
                                                        p { class: "m-0 mt-0.5 text-xs text-[var(--app-muted)]",
                                                            if on { "Connected" } else { "Not connected" }
                                                            if n > 0 { " · {n} open" }
                                                        }
                                                        p { class: "m-0 mt-1 text-xs text-[var(--app-muted)]",
                                                            "{app.outcome}"
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
