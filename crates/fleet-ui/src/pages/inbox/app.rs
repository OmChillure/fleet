//! Dispatch to the per-app inbox page.

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

use super::api::{InboxResponse, TicketRow, list_inbox};
use super::catalog::{explain_error, hook_url_for, inbox_app};
use super::shared::{PageData, apply_rows};
use super::views;
use crate::Route;
use crate::pages::integrations::list_integrations;

fn filter_value(provider: &str, resp: &InboxResponse) -> String {
    let Some(m) = resp.monitor.as_ref() else {
        return String::new();
    };
    match provider {
        "github" => m.github_repos.join(", "),
        "slack" => m.slack_channels.join(", "),
        "jira" => m.jira_jql.clone().unwrap_or_default(),
        "notion" => m.notion_databases.join(", "),
        "gitlab" => m.gitlab_projects.join(", "),
        "linear" => m.linear_teams.join(", "),
        "trello" => m.trello_boards.join(", "),
        _ => String::new(),
    }
}

#[component]
pub fn InboxApp(workspace_id: String, provider: String) -> Element {
    let provider = provider.trim().to_ascii_lowercase();
    let info = inbox_app(&provider);

    let rows = use_signal(Vec::<TicketRow>::new);
    let mut filter = use_signal(String::new);
    let mut status_line = use_signal(|| "Loading…".to_string());
    let mut last_error = use_signal(|| None::<String>);
    let mut hook_url = use_signal(String::new);
    let mut connected = use_signal(|| false);
    let mut account = use_signal(String::new);
    let busy = use_signal(|| false);
    let finish_error = use_signal(|| None::<(String, String)>);
    let mut seeded = use_signal(|| false);

    let ws_list = workspace_id.clone();
    let prov_list = provider.clone();
    use_future(move || {
        let ws = ws_list.clone();
        let prov = prov_list.clone();
        async move {
            loop {
                match list_inbox(&ws).await {
                    Ok(resp) => {
                        if let Some(m) = resp.monitor.as_ref() {
                            hook_url.set(hook_url_for(&m.hook_url, &prov));
                            last_error.set(
                                m.last_error
                                    .as_deref()
                                    .and_then(|e| explain_error(e, &prov)),
                            );
                        } else {
                            last_error.set(None);
                        }
                        if !seeded() {
                            filter.set(filter_value(&prov, &resp));
                            seeded.set(true);
                        }
                        if let Some(info) = inbox_app(&prov) {
                            apply_rows(
                                resp,
                                &prov,
                                PageData {
                                    info,
                                    rows,
                                    filter,
                                    status_line,
                                    last_error,
                                    hook_url,
                                    connected,
                                    account,
                                    busy,
                                    finish_error,
                                },
                            );
                        }
                    }
                    Err(e) => status_line.set(e.message()),
                }
                if let Ok(list) = list_integrations(&ws).await {
                    if let Some(i) = list.iter().find(|i| i.id.eq_ignore_ascii_case(&prov)) {
                        connected.set(i.connected);
                        account.set(
                            i.connection
                                .as_ref()
                                .and_then(|c| c.account_label.clone())
                                .unwrap_or_default(),
                        );
                    } else {
                        connected.set(false);
                    }
                }
                TimeoutFuture::new(8000).await;
            }
        }
    });

    let Some(info) = info else {
        return rsx! {
            div { class: "mx-auto max-w-4xl p-5",
                p { class: "text-sm text-[var(--app-muted)]",
                    "\"{provider}\" is not an inbox app."
                }
                Link {
                    to: Route::Inbox { workspace_id: workspace_id.clone() },
                    class: "mt-2 inline-block text-sm text-[var(--app-fg)]",
                    "Back to Inbox"
                }
            }
        };
    };

    let data = PageData {
        info,
        rows,
        filter,
        status_line,
        last_error,
        hook_url,
        connected,
        account,
        busy,
        finish_error,
    };
    views::render(&workspace_id, info, data)
}
