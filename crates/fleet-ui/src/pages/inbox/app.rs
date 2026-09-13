//! One inbox app: that provider's filters, copy, and item layout.

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

use super::api::{
    InboxResponse, MonitorPatch, TicketRow, finish_ticket, list_inbox, poll_inbox, update_monitor,
};
use super::catalog::inbox_app;
use super::experience::{experience, repo_issue_parts, row_kicker, slack_parts};
use crate::components::toast::show_toast;
use crate::pages::integrations::IntegrationLogo;
use crate::Route;

fn csv_list(raw: String) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn filter_value(provider: &str, resp: &InboxResponse) -> String {
    let Some(m) = resp.monitor.as_ref() else {
        return String::new();
    };
    match provider {
        "github" | "bitbucket" => m.github_repos.join(", "),
        "slack" | "teams" => m.slack_channels.join(", "),
        "jira" => m.jira_jql.clone().unwrap_or_default(),
        "notion" => m.notion_databases.join(", "),
        "gitlab" => m.gitlab_projects.join(", "),
        "linear" => m.linear_teams.join(", "),
        "trello" => m.trello_boards.join(", "),
        _ => String::new(),
    }
}

fn patch_for(provider: &str, raw: String) -> MonitorPatch {
    let list = csv_list(raw.clone());
    match provider {
        "github" | "bitbucket" => MonitorPatch {
            github_repos: Some(list),
            ..MonitorPatch::default()
        },
        "slack" | "teams" => MonitorPatch {
            slack_channels: Some(list),
            ..MonitorPatch::default()
        },
        "jira" => MonitorPatch {
            jira_jql: Some(raw),
            ..MonitorPatch::default()
        },
        "notion" => MonitorPatch {
            notion_databases: Some(list),
            ..MonitorPatch::default()
        },
        "gitlab" => MonitorPatch {
            gitlab_projects: Some(list),
            ..MonitorPatch::default()
        },
        "linear" => MonitorPatch {
            linear_teams: Some(list),
            ..MonitorPatch::default()
        },
        "trello" => MonitorPatch {
            trello_boards: Some(list),
            ..MonitorPatch::default()
        },
        _ => MonitorPatch::default(),
    }
}

#[component]
pub fn InboxApp(workspace_id: String, provider: String) -> Element {
    let provider = provider.trim().to_ascii_lowercase();
    let info = inbox_app(&provider);

    let mut rows = use_signal(Vec::<TicketRow>::new);
    let mut filter = use_signal(String::new);
    let mut status_line = use_signal(|| "Loading…".to_string());
    let mut last_error = use_signal(|| None::<String>);
    let mut hook_url = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut finish_error = use_signal(|| None::<(String, String)>);
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
                        if !seeded() {
                            filter.set(filter_value(&prov, &resp));
                            if let Some(m) = resp.monitor.as_ref() {
                                hook_url.set(m.hook_url.clone());
                                last_error.set(m.last_error.clone());
                            }
                            seeded.set(true);
                        } else if let Some(m) = resp.monitor.as_ref() {
                            last_error.set(m.last_error.clone());
                        }
                        let mine: Vec<_> = resp
                            .tickets
                            .into_iter()
                            .filter(|t| t.provider.eq_ignore_ascii_case(&prov))
                            .collect();
                        status_line.set(format!("{} item(s)", mine.len()));
                        rows.set(mine);
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

    let exp = experience(info);
    let hook = hook_url();
    let hook_for_app = hook.replace("{{provider}}", info.id);
    let chips = csv_list(filter());

    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-4xl flex-col gap-4 overflow-auto p-4 md:p-5",
            div { class: "flex flex-wrap items-start justify-between gap-3",
                div { class: "flex min-w-0 items-start gap-3",
                    IntegrationLogo { icon: info.id.to_string(), name: info.name.to_string() }
                    div {
                        Link {
                            to: Route::Inbox { workspace_id: workspace_id.clone() },
                            class: "text-xs text-[var(--app-muted)] no-underline hover:underline",
                            "Inbox"
                        }
                        h1 { class: "m-0 text-2xl font-bold tracking-tight text-[var(--app-fg)]",
                            "{info.name}"
                        }
                        p { class: "mt-1 text-sm text-[var(--app-muted)]", "{info.blurb}" }
                        p { class: "mt-0.5 font-mono text-xs text-[var(--app-muted)]", "{status_line}" }
                        if let Some(err) = last_error() {
                            p { class: "mt-1 text-xs text-red-400", "{err}" }
                        }
                    }
                }
                button {
                    class: "rounded-lg border border-[var(--app-border)] px-3 py-2 text-sm text-[var(--app-fg)] disabled:opacity-50",
                    r#type: "button",
                    disabled: busy(),
                    onclick: {
                        let ws = workspace_id.clone();
                        let prov = provider.clone();
                        move |_| {
                            if busy() { return; }
                            busy.set(true);
                            let ws = ws.clone();
                            let prov = prov.clone();
                            spawn(async move {
                                match poll_inbox(&ws).await {
                                    Ok(_) => show_toast().success("Polled."),
                                    Err(e) => show_toast().error(e.message()),
                                }
                                if let Ok(resp) = list_inbox(&ws).await {
                                    let mine: Vec<_> = resp
                                        .tickets
                                        .into_iter()
                                        .filter(|t| t.provider.eq_ignore_ascii_case(&prov))
                                        .collect();
                                    status_line.set(format!("{} item(s)", mine.len()));
                                    rows.set(mine);
                                }
                                busy.set(false);
                            });
                        }
                    },
                    "Refresh now"
                }
            }

            section { class: "rounded-2xl border {exp.accent} bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                    match info.id {
                        "slack" => "Channels the bot is in",
                        "teams" => "Teams channels",
                        "github" => "Repositories",
                        "gitlab" => "Projects",
                        "bitbucket" => "Repositories",
                        "jira" => "JQL",
                        "linear" => "Teams",
                        "trello" => "Boards",
                        "notion" => "Databases",
                        "confluence" => "Spaces",
                        "asana" => "Projects",
                        _ => "Filters",
                    }
                }
                p { class: "mt-1 text-xs text-[var(--app-muted)]", "{info.filter_hint}" }
                if info.id == "jira" {
                    textarea {
                        class: "mt-3 min-h-20 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 font-mono text-sm text-[var(--app-fg)]",
                        value: "{filter}",
                        placeholder: "statusCategory != Done ORDER BY updated DESC",
                        oninput: move |e| filter.set(e.value()),
                    }
                } else {
                    if !chips.is_empty() {
                        ul { class: "mt-3 m-0 flex list-none flex-wrap gap-1.5 p-0",
                            for c in chips {
                                li { class: "rounded-full border border-[var(--app-border)] px-2 py-0.5 font-mono text-[11px] text-[var(--app-fg)]",
                                    "{c}"
                                }
                            }
                        }
                    }
                    label { class: "mt-3 block text-[10px] uppercase tracking-wide text-[var(--app-muted)]",
                        "{info.filter_label}"
                    }
                    input {
                        class: "mt-1 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                        value: "{filter}",
                        oninput: move |e| filter.set(e.value()),
                    }
                }
                if !hook_for_app.is_empty() {
                    label { class: "mt-3 block text-[10px] uppercase tracking-wide text-[var(--app-muted)]",
                        "Webhook for {info.name}"
                    }
                    p { class: "mt-1 break-all font-mono text-[11px] text-[var(--app-fg)]",
                        "{hook_for_app}"
                    }
                }
                button {
                    class: "mt-3 rounded-lg border border-[var(--app-border)] px-3 py-2 text-sm text-[var(--app-fg)] disabled:opacity-50",
                    r#type: "button",
                    disabled: busy(),
                    onclick: {
                        let ws = workspace_id.clone();
                        let prov = provider.clone();
                        move |_| {
                            if busy() { return; }
                            busy.set(true);
                            let ws = ws.clone();
                            let patch = patch_for(&prov, filter());
                            spawn(async move {
                                match update_monitor(&ws, &patch).await {
                                    Ok(_) => show_toast().success("Saved."),
                                    Err(e) => show_toast().error(e.message()),
                                }
                                busy.set(false);
                            });
                        }
                    },
                    "Save"
                }
            }

            if rows().is_empty() {
                p { class: "rounded-2xl border border-dashed {exp.accent} p-8 text-center text-sm text-[var(--app-muted)]",
                    "{exp.empty}"
                }
            } else {
                ul { class: "m-0 list-none space-y-2 p-0",
                    for t in rows() {
                        {
                            let kicker = row_kicker(info.id, &t);
                            let action = exp.action;
                            let preview = t
                                .summary
                                .clone()
                                .filter(|s| !s.is_empty() && s != &t.title);
                            let (ch, _) = slack_parts(&t.key);
                            let (repo, num) = repo_issue_parts(&t.key);
                            let is_msg = matches!(info.id, "slack" | "teams");
                            let is_repo = matches!(info.id, "github" | "gitlab" | "bitbucket");
                            let is_issue_key = matches!(info.id, "jira" | "linear");
                            rsx! {
                                li {
                                    key: "{t.provider}:{t.key}",
                                    class: "flex flex-wrap items-start justify-between gap-3 rounded-xl border {exp.accent} bg-[var(--app-bg-elevated)] px-4 py-3",
                                    div { class: "min-w-0 flex-1",
                                        if is_msg {
                                            p { class: "m-0 font-mono text-[11px] text-[var(--app-muted)]",
                                                "#{ch}"
                                            }
                                            p { class: "m-0 mt-1 text-sm text-[var(--app-fg)]",
                                                "{t.title}"
                                            }
                                        }
                                        if is_repo {
                                            p { class: "m-0 text-sm font-medium text-[var(--app-fg)]",
                                                span { class: "font-mono text-[var(--app-muted)]", "{num}" }
                                                "  {t.title}"
                                            }
                                            p { class: "m-0 mt-0.5 font-mono text-[11px] text-[var(--app-muted)]",
                                                "{repo} · {t.state}"
                                            }
                                        }
                                        if is_issue_key {
                                            p { class: "m-0 text-sm font-medium text-[var(--app-fg)]",
                                                span { class: "mr-2 inline-block rounded bg-[var(--app-bg)] px-1.5 py-0.5 font-mono text-[11px]",
                                                    "{t.key}"
                                                }
                                                "{t.title}"
                                            }
                                            p { class: "m-0 mt-0.5 font-mono text-[11px] text-[var(--app-muted)]",
                                                "{t.state}"
                                            }
                                        }
                                        if !is_msg && !is_repo && !is_issue_key {
                                            p { class: "m-0 text-sm font-medium text-[var(--app-fg)]",
                                                "{t.title}"
                                            }
                                            p { class: "m-0 mt-0.5 font-mono text-[11px] text-[var(--app-muted)]",
                                                "{kicker}"
                                            }
                                        }
                                        if let Some(prev) = preview {
                                            p { class: "m-0 mt-1 line-clamp-2 text-xs text-[var(--app-muted)]",
                                                "{prev}"
                                            }
                                        }
                                        if let Some(url) = &t.url {
                                            a {
                                                class: "mt-1 inline-block text-[11px] text-[var(--app-muted)] underline",
                                                href: "{url}",
                                                target: "_blank",
                                                rel: "noreferrer",
                                                "Open in {info.name}"
                                            }
                                        }
                                    }
                                    if t.run_id.is_none() && t.state == "open" {
                                        div { class: "flex min-w-0 flex-col items-end gap-1",
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
                                                        finish_error.set(None);
                                                        let ws = ws.clone();
                                                        let provider = provider.clone();
                                                        let key = key.clone();
                                                        spawn(async move {
                                                            match finish_ticket(&ws, &provider, &key).await {
                                                                Ok(_) => show_toast().success("Fleet launched."),
                                                                Err(e) => {
                                                                    finish_error.set(Some((
                                                                        format!("{provider}:{key}"),
                                                                        e.message(),
                                                                    )));
                                                                }
                                                            }
                                                            if let Ok(resp) = list_inbox(&ws).await {
                                                                let mine: Vec<_> = resp
                                                                    .tickets
                                                                    .into_iter()
                                                                    .filter(|t| t.provider.eq_ignore_ascii_case(&provider))
                                                                    .collect();
                                                                status_line.set(format!("{} item(s)", mine.len()));
                                                                rows.set(mine);
                                                            }
                                                            busy.set(false);
                                                        });
                                                    }
                                                },
                                                "{action}"
                                            }
                                            if let Some((id, msg)) = finish_error() {
                                                if id == format!("{}:{}", t.provider, t.key) {
                                                    p { class: "m-0 max-w-56 text-right text-[11px] text-red-400",
                                                        "{msg}"
                                                    }
                                                }
                                            }
                                        }
                                    } else if let Some(run) = &t.run_id {
                                        span { class: "font-mono text-[11px] text-[var(--app-muted)]",
                                            "run {run}"
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
