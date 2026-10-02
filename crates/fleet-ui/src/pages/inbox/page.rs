//! Inbox index: work-tools hub, grouped the way a team already uses the apps.

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

use super::api::{InboxResponse, list_inbox, poll_inbox, start_monitor, stop_monitor};
use super::catalog::INBOX_APPS;
use crate::Route;
use crate::components::toast::show_toast;
use crate::pages::integrations::{
    IntegrationCategory, IntegrationLogo, live_connections, load_integrations,
};

#[derive(Clone, PartialEq)]
struct InboxEntry {
    provider: String,
    connection_id: String,
    category_id: String,
    name: String,
    failed: bool,
}

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
    let mut connections = use_signal(Vec::<InboxEntry>::new);
    let mut categories = use_signal(Vec::<IntegrationCategory>::new);
    let mut monitor_on = use_signal(|| false);
    let mut busy = use_signal(|| false);

    let ws_list = workspace_id.clone();
    use_future(move || {
        let ws = ws_list.clone();
        async move {
            loop {
                let mut next_counts = std::collections::BTreeMap::new();
                let mut next_conn = Vec::new();
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
                if let Ok(snapshot) = load_integrations(&ws).await {
                    categories.set(snapshot.categories);
                    for i in snapshot.integrations {
                        let provider = i.id.to_ascii_lowercase();
                        if !INBOX_APPS.iter().any(|a| a.id == provider) {
                            continue;
                        }
                        for conn in live_connections(&i) {
                            let failed = conn.status.eq_ignore_ascii_case("error");
                            let on = !failed
                                && (conn.connected
                                    || conn.status.eq_ignore_ascii_case("connected"));
                            if !on && !failed {
                                continue;
                            }
                            let name = if conn.display_name.trim().is_empty() {
                                i.name.clone()
                            } else {
                                conn.display_name.clone()
                            };
                            next_conn.push(InboxEntry {
                                provider: provider.clone(),
                                connection_id: conn.id.clone(),
                                category_id: i.category_id.clone(),
                                name,
                                failed,
                            });
                        }
                    }
                }
                counts.set(next_counts);
                connections.set(next_conn);
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
                "Start monitor checks connected apps on a schedule. Refresh now checks once. Connect another account from Integrations."
            }
            if connections().is_empty() {
                p { class: "m-0 text-sm text-[var(--app-muted)]",
                    "No connected apps yet. "
                    Link {
                        to: Route::Integrations { workspace_id: workspace_id.clone() },
                        class: "text-[var(--app-fg)] underline",
                        "Add one in Integrations"
                    }
                }
            }

            for group in categories() {
                {
                    let rows: Vec<_> = connections()
                        .into_iter()
                        .filter(|c| c.category_id == group.id)
                        .collect();
                    if rows.is_empty() {
                        rsx! { div { key: "{group.id}" } }
                    } else {
                        rsx! {
                            section { key: "{group.id}",
                                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                                    "{group.label}"
                                }
                                p { class: "mt-1 mb-3 text-xs text-[var(--app-muted)]",
                                    "{group.subtitle}"
                                }
                                ul { class: "m-0 grid list-none gap-3 p-0 sm:grid-cols-2",
                                    for row in rows {
                                        {
                                            let app = INBOX_APPS.iter().find(|a| a.id == row.provider);
                                            let n = *counts().get(row.provider.as_str()).unwrap_or(&0);
                                            let title = row.name.clone();
                                            let provider = row.provider.clone();
                                            let connection_id = row.connection_id.clone();
                                            let failed = row.failed;
                                            let border = if failed {
                                                "border-red-500/60"
                                            } else {
                                                "border-[var(--app-border)]"
                                            };
                                            rsx! {
                                                li { key: "{connection_id}",
                                                    div { class: "flex h-full flex-col gap-2 rounded-2xl border bg-[var(--app-bg-elevated)] p-4 {border}",
                                                        Link {
                                                            to: Route::InboxConnection {
                                                                workspace_id: workspace_id.clone(),
                                                                provider: provider.clone(),
                                                                connection_id: connection_id.clone(),
                                                            },
                                                            class: "flex items-start gap-3 text-inherit no-underline hover:opacity-90",
                                                            IntegrationLogo {
                                                                icon: provider.clone(),
                                                                name: app.map(|a| a.name.to_string()).unwrap_or_else(|| provider.clone()),
                                                            }
                                                            div { class: "min-w-0",
                                                                p { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                                                                    "{title}"
                                                                }
                                                                p { class: "m-0 mt-0.5 text-xs text-[var(--app-muted)]",
                                                                    if failed { "Sign-in failed" } else { "Connected" }
                                                                    if n > 0 { " · {n} open" }
                                                                }
                                                                if let Some(app) = app {
                                                                    p { class: "m-0 mt-1 text-xs text-[var(--app-muted)]",
                                                                        "{app.outcome}"
                                                                    }
                                                                }
                                                            }
                                                        }
                                                        Link {
                                                            to: Route::ConnectionEdit {
                                                                workspace_id: workspace_id.clone(),
                                                                provider: provider.clone(),
                                                                connection_id,
                                                            },
                                                            class: "text-xs text-[var(--app-accent)] underline",
                                                            "Manage"
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
