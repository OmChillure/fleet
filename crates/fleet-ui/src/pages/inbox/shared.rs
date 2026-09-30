//! Shared chrome for per-app inbox pages. Layout still belongs to each view.

use dioxus::prelude::*;

use super::api::{
    finish_ticket, list_inbox, poll_inbox, update_monitor, InboxResponse, MonitorPatch, TicketRow,
};
use super::catalog::{explain_error, InboxAppInfo};
use super::connect::ConnectApp;
use super::experience::experience;
use crate::clipboard::copy_text;
use crate::components::toast::show_toast;
use crate::pages::integrations::{IntegrationLogo, message_is_auth_failure};
use crate::Route;

#[derive(Clone, Copy)]
pub struct PageData {
    pub info: &'static InboxAppInfo,
    pub rows: Signal<Vec<TicketRow>>,
    pub filter: Signal<String>,
    pub status_line: Signal<String>,
    pub last_error: Signal<Option<String>>,
    pub hook_url: Signal<String>,
    pub connected: Signal<bool>,
    pub auth_failed: Signal<bool>,
    pub account: Signal<String>,
    pub allowed: Signal<Vec<String>>,
    pub auto_launch: Signal<bool>,
    pub busy: Signal<bool>,
    pub finish_error: Signal<Option<(String, String)>>,
}

impl PageData {
    pub fn rows(self) -> Vec<TicketRow> {
        (self.rows)()
    }
    pub fn filter(self) -> String {
        (self.filter)()
    }
    pub fn status_line(self) -> String {
        (self.status_line)()
    }
    pub fn last_error(self) -> Option<String> {
        (self.last_error)()
    }
    pub fn hook_url(self) -> String {
        (self.hook_url)()
    }
    pub fn connected(self) -> bool {
        (self.connected)()
    }
    pub fn auth_failed(self) -> bool {
        (self.auth_failed)()
    }
    pub fn allowed(self) -> Vec<String> {
        (self.allowed)()
    }
    pub fn auto_launch(self) -> bool {
        (self.auto_launch)()
    }
    pub fn busy(self) -> bool {
        (self.busy)()
    }
    pub fn finish_error(self) -> Option<(String, String)> {
        (self.finish_error)()
    }
}

pub fn csv_list(raw: String) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

pub fn patch_for(provider: &str, raw: String) -> MonitorPatch {
    use super::experience::{
        normalize_channel, normalize_notion_id, normalize_owner_name, normalize_trello_board,
    };
    let list = csv_list(raw.clone());
    match provider {
        "github" => MonitorPatch {
            github_repos: Some(
                list.into_iter()
                    .filter_map(|s| normalize_owner_name(&s).or(Some(s)))
                    .collect(),
            ),
            ..MonitorPatch::default()
        },
        "gitea" => MonitorPatch {
            gitea_repos: Some(
                list.into_iter()
                    .filter_map(|s| normalize_owner_name(&s).or(Some(s)))
                    .collect(),
            ),
            ..MonitorPatch::default()
        },
        "gogs" => MonitorPatch {
            gogs_repos: Some(
                list.into_iter()
                    .filter_map(|s| normalize_owner_name(&s).or(Some(s)))
                    .collect(),
            ),
            ..MonitorPatch::default()
        },
        "gitlab" => MonitorPatch {
            gitlab_projects: Some(
                list.into_iter()
                    .filter_map(|s| normalize_owner_name(&s).or(Some(s)))
                    .collect(),
            ),
            ..MonitorPatch::default()
        },
        "slack" => MonitorPatch {
            slack_channels: Some(
                list.into_iter()
                    .filter_map(|s| normalize_channel(&s))
                    .collect(),
            ),
            ..MonitorPatch::default()
        },
        "jira" => MonitorPatch {
            jira_jql: Some(raw),
            ..MonitorPatch::default()
        },
        "notion" => MonitorPatch {
            notion_databases: Some(
                list.into_iter()
                    .filter_map(|s| normalize_notion_id(&s))
                    .collect(),
            ),
            ..MonitorPatch::default()
        },
        "linear" => MonitorPatch {
            linear_teams: Some(list),
            ..MonitorPatch::default()
        },
        "trello" => MonitorPatch {
            trello_boards: Some(
                list.into_iter()
                    .filter_map(|s| normalize_trello_board(&s))
                    .collect(),
            ),
            ..MonitorPatch::default()
        },
        _ => MonitorPatch::default(),
    }
}

pub fn apply_rows(resp: InboxResponse, provider: &str, mut data: PageData) {
    let mine: Vec<_> = resp
        .tickets
        .into_iter()
        .filter(|t| t.provider.eq_ignore_ascii_case(provider))
        .collect();
    let word = experience(data.info).item_word;
    data.status_line.set(if mine.is_empty() {
        format!("No {word}s yet")
    } else if mine.len() == 1 {
        format!("1 {word} waiting")
    } else {
        format!("{} {word}s waiting", mine.len())
    });
    data.rows.set(mine);
}

pub fn refresh_now(ws: String, provider: String, mut data: PageData) {
    if data.busy() {
        return;
    }
    data.busy.set(true);
    let name = data.info.name;
    spawn(async move {
        match poll_inbox(&ws).await {
            Ok(_) => show_toast().success(format!("Checked {name}.")),
            Err(e) => show_toast().error(e.message()),
        }
        if let Ok(resp) = list_inbox(&ws).await {
            if let Some(m) = resp.monitor.as_ref() {
                data.last_error.set(
                    m.last_error
                        .as_deref()
                        .and_then(|e| explain_error(e, &provider)),
                );
            }
            apply_rows(resp, &provider, data);
        }
        data.busy.set(false);
    });
}

pub fn save_filter(ws: String, patch: MonitorPatch, mut busy: Signal<bool>, ok: &'static str) {
    if busy() {
        return;
    }
    busy.set(true);
    spawn(async move {
        match update_monitor(&ws, &patch).await {
            Ok(_) => show_toast().success(ok),
            Err(e) => show_toast().error(e.message()),
        }
        busy.set(false);
    });
}

pub fn launch_item(ws: String, t: &TicketRow, mut data: PageData) {
    if data.busy() {
        return;
    }
    data.busy.set(true);
    data.finish_error.set(None);
    let provider = t.provider.clone();
    let key = t.key.clone();
    spawn(async move {
        match finish_ticket(&ws, &provider, &key).await {
            Ok(_) => show_toast().success("Fleet launched."),
            Err(e) => {
                data.finish_error
                    .set(Some((format!("{provider}:{key}"), e.message())));
            }
        }
        if let Ok(resp) = list_inbox(&ws).await {
            apply_rows(resp, &provider, data);
        }
        data.busy.set(false);
    });
}

pub fn frame(inner: Element) -> Element {
    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-7xl flex-col gap-4 overflow-auto p-4 md:p-5",
            {inner}
        }
    }
}

pub fn header(ws: &str, data: PageData, extra: Element) -> Element {
    let info = data.info;
    let exp = experience(info);
    let on = data.connected();
    let broken = data.auth_failed()
        || data
            .last_error()
            .as_deref()
            .is_some_and(message_is_auth_failure);
    let healthy = on && !broken;
    let status = data.status_line();
    let provider = info.id.to_string();
    let ws_refresh = ws.to_string();
    rsx! {
        div { class: "flex flex-wrap items-start justify-between gap-3",
            div { class: "flex min-w-0 items-start gap-3",
                IntegrationLogo { icon: info.id.to_string(), name: info.name.to_string() }
                div {
                    Link {
                        to: Route::Inbox { workspace_id: ws.to_string() },
                        class: "text-xs text-[var(--app-muted)] no-underline hover:underline",
                        "Inbox"
                    }
                    h1 { class: "m-0 text-2xl font-bold tracking-tight text-[var(--app-fg)]",
                        "{info.name}"
                    }
                    p { class: "mt-1 max-w-xl text-sm text-[var(--app-muted)]", "{info.work}" }
                    p { class: "mt-1 text-xs text-[var(--app-muted)]",
                        if broken {
                            "Sign-in failed · "
                        } else if healthy {
                            "Connected · "
                        } else {
                            "Not connected yet · "
                        }
                        "{status}"
                    }
                }
            }
            div { class: "flex flex-wrap items-center gap-2",
                ConnectApp {
                    workspace_id: ws.to_string(),
                    provider: provider.clone(),
                    name: info.name.to_string(),
                    connected: data.connected,
                    auth_failed: data.auth_failed,
                    account: data.account,
                }
                button {
                    class: "rounded-lg border border-[var(--app-border)] px-3 py-2 text-sm text-[var(--app-fg)] disabled:opacity-50",
                    r#type: "button",
                    disabled: data.busy(),
                    onclick: move |_| refresh_now(ws_refresh.clone(), provider.clone(), data),
                    "Refresh now"
                }
            }
        }
        p { class: "m-0 text-sm text-[var(--app-fg)]", "{info.fleet_does}" }
        p { class: "m-0 -mt-2 text-xs text-[var(--app-muted)]",
            span { class: "font-medium {exp.mark}", "What you get. " }
            "{info.outcome}"
        }
        {extra}
    }
}

pub fn error_banner(data: PageData) -> Element {
    if let Some(err) = data.last_error() {
        rsx! {
            p { class: "rounded-xl border border-red-500/30 bg-red-500/5 px-4 py-3 text-sm text-red-300",
                "{err}"
            }
        }
    } else {
        rsx! {}
    }
}

pub fn need_connect(ws: &str, data: PageData) -> Element {
    if data.connected() || data.auth_failed() {
        return rsx! {};
    }
    let info = data.info;
    rsx! {
        p { class: "rounded-xl border border-amber-500/30 bg-amber-500/5 px-4 py-3 text-sm text-[var(--app-fg)]",
            "This page stays empty until {info.name} is connected. Use Connect {info.name} above, or "
            Link {
                to: Route::Integrations { workspace_id: ws.to_string() },
                class: "font-medium underline",
                "open Integrations"
            }
            "."
        }
    }
}

pub fn flow(data: PageData) -> Element {
    let info = data.info;
    let exp = experience(info);
    rsx! {
        ol { class: "m-0 grid list-none gap-2 p-0 sm:grid-cols-3",
            for (i, step) in info.flow.iter().enumerate() {
                li { class: "rounded-xl border {exp.accent} {exp.wash} px-3 py-2",
                    p { class: "m-0 text-[10px] uppercase tracking-wide {exp.mark}",
                        "Step {i + 1}"
                    }
                    p { class: "m-0 mt-1 text-xs text-[var(--app-fg)]", "{step}" }
                }
            }
        }
    }
}

pub fn setup(data: PageData, heading: &str) -> Element {
    let info = data.info;
    let exp = experience(info);
    rsx! {
        section { class: "rounded-2xl border {exp.accent} bg-[var(--app-bg-elevated)] p-4",
            h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "{heading}" }
            ol { class: "mt-3 mb-0 list-decimal space-y-2 pl-5 text-sm text-[var(--app-fg)]",
                for (i, step) in info.steps.iter().enumerate() {
                    li { key: "{i}", "{step}" }
                }
            }
        }
    }
}

pub fn hook_card(data: PageData) -> Element {
    let url = data.hook_url();
    if url.is_empty() {
        return rsx! {};
    }
    let info = data.info;
    let exp = experience(info);
    rsx! {
        section { class: "rounded-2xl border {exp.accent} bg-[var(--app-bg-elevated)] p-4",
            h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "{info.hook_title}" }
            p { class: "mt-1 text-xs text-[var(--app-muted)]", "{info.hook_help}" }
            p { class: "mt-3 break-all font-mono text-[11px] text-[var(--app-fg)]", "{url}" }
            button {
                class: "mt-2 rounded-lg border border-[var(--app-border)] px-3 py-1.5 text-sm text-[var(--app-fg)]",
                r#type: "button",
                onclick: move |_| {
                    if copy_text(&url) {
                        show_toast().success(format!("Copied the {} URL.", info.name));
                    } else {
                        show_toast().error("Could not copy. Select the URL and copy it yourself.");
                    }
                },
                "Copy URL"
            }
        }
    }
}

pub fn filter_box(ws: &str, mut data: PageData, hint: &str, multiline: bool) -> Element {
    if !data.info.has_filter {
        return rsx! {};
    }
    let info = data.info;
    let exp = experience(info);
    let chips = csv_list(data.filter());
    let examples = info.examples;
    let ws = ws.to_string();
    let ok = info.filter_save;
    let provider = info.id;
    rsx! {
        section { class: "rounded-2xl border {exp.accent} bg-[var(--app-bg-elevated)] p-4",
            h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "{info.filter_label}" }
            p { class: "mt-1 text-xs text-[var(--app-muted)]", "{hint}" }
            if !examples.is_empty() {
                ul { class: "mt-2 m-0 flex list-none flex-wrap gap-1.5 p-0",
                    for ex in examples {
                        li {
                            button {
                                class: "rounded-full border border-[var(--app-border)] px-2 py-0.5 font-mono text-[11px] text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)]",
                                r#type: "button",
                                onclick: {
                                    let ex = (*ex).to_string();
                                    move |_| data.filter.set(ex.clone())
                                },
                                "{ex}"
                            }
                        }
                    }
                }
            }
            if !multiline && !chips.is_empty() {
                ul { class: "mt-3 m-0 flex list-none flex-wrap gap-1.5 p-0",
                    for c in chips {
                        li { class: "rounded-full border {exp.accent} {exp.wash} px-2 py-0.5 font-mono text-[11px] text-[var(--app-fg)]",
                            "{c}"
                        }
                    }
                }
            }
            if multiline {
                textarea {
                    class: "mt-3 min-h-24 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 font-mono text-sm text-[var(--app-fg)]",
                    value: "{data.filter}",
                    placeholder: "{info.filter_placeholder}",
                    oninput: move |e| data.filter.set(e.value()),
                }
            } else {
                input {
                    class: "mt-3 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                    value: "{data.filter}",
                    placeholder: "{info.filter_placeholder}",
                    oninput: move |e| data.filter.set(e.value()),
                }
            }
            button {
                class: "mt-3 rounded-lg border border-[var(--app-border)] px-3 py-2 text-sm text-[var(--app-fg)] disabled:opacity-50",
                r#type: "button",
                disabled: data.busy(),
                onclick: move |_| {
                    save_filter(ws.clone(), patch_for(provider, data.filter()), data.busy, ok)
                },
                "{info.filter_save}"
            }
        }
    }
}

pub fn empty_or_list(data: PageData, list: Element) -> Element {
    if data.rows().is_empty() {
        let info = data.info;
        let exp = experience(info);
        rsx! {
            p { class: "rounded-2xl border border-dashed {exp.accent} p-8 text-center text-sm text-[var(--app-muted)]",
                "{info.empty}"
            }
        }
    } else {
        list
    }
}

pub fn action_cell(ws: &str, t: &TicketRow, data: PageData) -> Element {
    let info = data.info;
    if t.run_id.is_none() && t.state == "open" {
        let ws = ws.to_string();
        let ticket = t.clone();
        rsx! {
            div { class: "flex min-w-0 flex-col items-end gap-1",
                button {
                    class: "rounded-lg bg-[var(--app-fg)] px-3 py-1.5 text-sm font-semibold text-[var(--app-bg)] disabled:opacity-50",
                    r#type: "button",
                    disabled: data.busy(),
                    onclick: move |_| launch_item(ws.clone(), &ticket, data),
                    "{info.action}"
                }
                if let Some((id, msg)) = data.finish_error() {
                    if id == format!("{}:{}", t.provider, t.key) {
                        p { class: "m-0 max-w-56 text-right text-[11px] text-red-400", "{msg}" }
                    }
                }
            }
        }
    } else if let Some(run) = &t.run_id {
        rsx! {
            span { class: "font-mono text-[11px] text-[var(--app-muted)]", "run {run}" }
        }
    } else {
        rsx! {}
    }
}

pub fn open_in(t: &TicketRow, name: &str) -> Element {
    if let Some(url) = &t.url {
        rsx! {
            a {
                class: "mt-1 inline-block text-[11px] text-[var(--app-muted)] underline",
                href: "{url}",
                target: "_blank",
                rel: "noreferrer",
                "Open in {name}"
            }
        }
    } else {
        rsx! {}
    }
}

pub fn preview(t: &TicketRow) -> Element {
    let preview = t.summary.clone().filter(|s| !s.is_empty() && s != &t.title);
    if let Some(prev) = preview {
        rsx! {
            p { class: "m-0 mt-1 line-clamp-2 text-xs text-[var(--app-muted)]", "{prev}" }
        }
    } else {
        rsx! {}
    }
}
