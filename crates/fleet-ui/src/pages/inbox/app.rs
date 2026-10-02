//! Dispatch to the per-app inbox page.

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

use super::api::{InboxResponse, TicketRow, list_inbox};
use super::catalog::{explain_error, hook_url_for, inbox_app};
use super::shared::{PageData, apply_rows};
use super::views;
use crate::Route;
use crate::pages::integrations::{
    find_integration, list_integrations, live_connections, message_is_auth_failure,
};

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
        "gitea" => m.gitea_repos.join(", "),
        "gogs" => m.gogs_repos.join(", "),
        "linear" => m.linear_teams.join(", "),
        "trello" => m.trello_boards.join(", "),
        _ => String::new(),
    }
}

#[component]
pub fn InboxConnection(workspace_id: String, provider: String, connection_id: String) -> Element {
    let key = format!("{workspace_id}:{provider}:{connection_id}");
    rsx! {
        InboxAppBody {
            key: "{key}",
            workspace_id,
            provider,
            connection_id,
        }
    }
}

#[component]
pub fn InboxApp(workspace_id: String, provider: String) -> Element {
    let key = format!("{workspace_id}:{provider}");
    rsx! {
        InboxProviderGate { key: "{key}", workspace_id, provider }
    }
}

#[component]
fn InboxProviderGate(workspace_id: String, provider: String) -> Element {
    let mut choices = use_signal(Vec::<(String, String, bool)>::new);
    let mut ready = use_signal(|| false);
    let nav = use_navigator();
    let ws = workspace_id.clone();
    let prov = provider.clone();
    use_future(move || {
        let ws = ws.clone();
        let prov = prov.clone();
        async move {
            if let Ok(list) = list_integrations(&ws).await {
                if let Some(integ) = find_integration(&list, &prov) {
                    let rows: Vec<_> = live_connections(integ)
                        .into_iter()
                        .map(|c| {
                            let failed = c.status.eq_ignore_ascii_case("error");
                            let title = if c.display_name.trim().is_empty() {
                                integ.name.clone()
                            } else {
                                c.display_name.clone()
                            };
                            (c.id, title, failed)
                        })
                        .collect();
                    if rows.len() == 1 {
                        nav.replace(Route::InboxConnection {
                            workspace_id: ws,
                            provider: prov,
                            connection_id: rows[0].0.clone(),
                        });
                        return;
                    }
                    choices.set(rows);
                }
            }
            ready.set(true);
        }
    });
    if !ready() {
        return rsx! { p { class: "p-5 text-sm text-[var(--app-muted)]", "Loading…" } };
    }
    rsx! {
        div { class: "mx-auto flex max-w-xl flex-col gap-3 p-5",
            h1 { class: "m-0 text-xl font-bold text-[var(--app-fg)]", "Choose a connection" }
            p { class: "m-0 text-sm text-[var(--app-muted)]",
                "This service has more than one account. Open the named one."
            }
            if choices().is_empty() {
                p { class: "m-0 text-sm text-[var(--app-muted)]", "Nothing is connected for this service." }
            }
            for (id, title, failed) in choices() {
                {
                    let tone = if failed {
                        "border-red-500/50 text-red-200"
                    } else {
                        "border-[var(--app-border)] text-[var(--app-fg)]"
                    };
                    rsx! {
                        Link {
                            key: "{id}",
                            to: Route::InboxConnection {
                                workspace_id: workspace_id.clone(),
                                provider: provider.clone(),
                                connection_id: id,
                            },
                            class: "rounded-xl border px-4 py-3 text-sm no-underline {tone}",
                            "{title}"
                            if failed { " — sign-in failed" }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn InboxAppBody(workspace_id: String, provider: String, connection_id: String) -> Element {
    // The router reuses this page when only the connection id changes.
    // Read the live route so the second Gitea is not stuck on the first.
    let route = use_route::<crate::Route>();
    let (workspace_id, provider, connection_id) = match route {
        crate::Route::InboxConnection {
            workspace_id,
            provider,
            connection_id,
        } => (workspace_id, provider, connection_id),
        _ => (workspace_id, provider, connection_id),
    };
    let provider = provider.trim().to_ascii_lowercase();
    let info = inbox_app(&provider);

    let mut rows = use_signal(Vec::<TicketRow>::new);
    let mut filter = use_signal(String::new);
    let mut status_line = use_signal(|| "Loading…".to_string());
    let mut last_error = use_signal(|| None::<String>);
    let mut hook_url = use_signal(String::new);
    let mut connected = use_signal(|| false);
    let mut auth_failed = use_signal(|| false);
    let mut account = use_signal(String::new);
    let mut connection_title = use_signal(String::new);
    let mut bound_connection = use_signal(|| connection_id.clone());
    let mut bound_provider = use_signal(|| provider.clone());
    let mut allowed = use_signal(Vec::<String>::new);
    let mut auto_launch = use_signal(|| false);
    let busy = use_signal(|| false);
    let finish_error = use_signal(|| None::<(String, String)>);
    let mut seeded = use_signal(|| false);

    if bound_connection() != connection_id || bound_provider() != provider {
        bound_connection.set(connection_id.clone());
        bound_provider.set(provider.clone());
        connection_title.set(String::new());
        auth_failed.set(false);
        connected.set(false);
        account.set(String::new());
        allowed.set(Vec::new());
        rows.set(Vec::new());
        seeded.set(false);
        status_line.set("Loading…".to_string());
        last_error.set(None);
    }

    let ws_conn = workspace_id.clone();
    use_future(move || {
        let ws = ws_conn.clone();
        async move {
            loop {
                let prov = bound_provider();
                match list_integrations(&ws).await {
                    Ok(list) => {
                        if let Some(i) = find_integration(&list, &prov) {
                            let wanted = bound_connection();
                            let picked = live_connections(i).into_iter().find(|c| c.id == wanted);
                            if let Some(conn) = picked {
                                let failed = conn.status.eq_ignore_ascii_case("error");
                                auth_failed.set(failed);
                                connected.set(
                                    !failed
                                        && (conn.connected
                                            || conn.status.eq_ignore_ascii_case("connected")),
                                );
                                account.set(conn.account_label.clone().unwrap_or_default());
                                connection_title.set(if conn.display_name.trim().is_empty() {
                                    i.name.clone()
                                } else {
                                    conn.display_name.clone()
                                });
                                allowed.set(crate::pages::integrations::granted_repos(&conn));
                            } else {
                                connected.set(false);
                                auth_failed.set(false);
                                account.set(String::new());
                                connection_title.set(String::new());
                                allowed.set(Vec::new());
                            }
                        } else {
                            connected.set(false);
                            auth_failed.set(false);
                            account.set(String::new());
                            allowed.set(Vec::new());
                        }
                    }
                    Err(_) => {}
                }
                TimeoutFuture::new(8000).await;
            }
        }
    });

    let ws_now = workspace_id.clone();
    use_effect(move || {
        let id = bound_connection();
        let prov = bound_provider();
        let ws = ws_now.clone();
        spawn(async move {
            let Ok(list) = list_integrations(&ws).await else {
                return;
            };
            if bound_connection() != id {
                return;
            }
            let Some(i) = find_integration(&list, &prov) else {
                return;
            };
            let Some(conn) = live_connections(i).into_iter().find(|c| c.id == id) else {
                return;
            };
            let failed = conn.status.eq_ignore_ascii_case("error");
            auth_failed.set(failed);
            connected
                .set(!failed && (conn.connected || conn.status.eq_ignore_ascii_case("connected")));
            account.set(conn.account_label.clone().unwrap_or_default());
            connection_title.set(if conn.display_name.trim().is_empty() {
                i.name.clone()
            } else {
                conn.display_name.clone()
            });
            allowed.set(crate::pages::integrations::granted_repos(&conn));
        });
    });

    let ws_list = workspace_id.clone();
    use_future(move || {
        let ws = ws_list.clone();
        async move {
            loop {
                let prov = bound_provider();
                match list_inbox(&ws).await {
                    Ok(resp) => {
                        if let Some(m) = resp.monitor.as_ref() {
                            hook_url.set(hook_url_for(&m.hook_url, &prov));
                            let explained = m
                                .last_error
                                .as_deref()
                                .and_then(|e| explain_error(e, &prov));
                            if explained.as_deref().is_some_and(message_is_auth_failure) {
                                auth_failed.set(true);
                                connected.set(false);
                            }
                            last_error.set(explained);
                        } else {
                            last_error.set(None);
                        }
                        if !seeded() {
                            filter.set(filter_value(&prov, &resp));
                            auto_launch.set(resp.monitor.as_ref().is_some_and(|m| m.auto_launch));
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
                                    auth_failed,
                                    account,
                                    allowed,
                                    auto_launch,
                                    busy,
                                    finish_error,
                                    connection_id: bound_connection,
                                    connection_title,
                                },
                            );
                        }
                    }
                    Err(e) => status_line.set(e.message()),
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
        auth_failed,
        account,
        allowed,
        auto_launch,
        busy,
        finish_error,
        connection_id: bound_connection,
        connection_title,
    };
    views::render(&workspace_id, info, data)
}
