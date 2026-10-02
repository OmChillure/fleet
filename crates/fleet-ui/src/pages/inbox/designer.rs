//! One workflow board for every inbox. Each app has its own board.
//! Blocks are shared. Only the narrow block changes shape with the app,
//! and only when that app actually has a filter.

use dioxus::prelude::*;

use super::api::MonitorPatch;
use super::catalog::InboxAppInfo;
use super::shared::{PageData, csv_list, patch_for, save_filter};
use crate::pages::inbox::experience::experience;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Source,
    Narrow,
    Act,
    Deliver,
}

pub fn pipeline(has_filter: bool) -> Vec<Role> {
    let mut roles = vec![Role::Source];
    if has_filter {
        roles.push(Role::Narrow);
    }
    roles.push(Role::Act);
    roles.push(Role::Deliver);
    roles
}

/// Empty stored list means every allowed repo. Unchecking one stores the rest.
/// Unchecking the last repo snaps back to every repo, because an empty list
/// is how the monitor says "all".
pub fn toggle_repo(stored: &[String], allowed: &[String], repo: &str) -> Vec<String> {
    if allowed.is_empty() {
        return Vec::new();
    }
    let mut set: Vec<String> = if stored.is_empty() {
        allowed.to_vec()
    } else {
        stored
            .iter()
            .filter(|s| allowed.iter().any(|a| a.eq_ignore_ascii_case(s)))
            .cloned()
            .collect()
    };
    if let Some(i) = set.iter().position(|s| s.eq_ignore_ascii_case(repo)) {
        set.remove(i);
    } else if allowed.iter().any(|a| a.eq_ignore_ascii_case(repo)) {
        set.push(repo.to_string());
    }
    let covers_all = allowed
        .iter()
        .all(|a| set.iter().any(|s| s.eq_ignore_ascii_case(a)));
    if set.is_empty() || covers_all {
        Vec::new()
    } else {
        set
    }
}

pub fn repo_checked(stored: &[String], repo: &str) -> bool {
    stored.is_empty() || stored.iter().any(|s| s.eq_ignore_ascii_case(repo))
}

pub fn default_positions(n: usize) -> Vec<(f64, f64)> {
    (0..n).map(|i| (24.0 + i as f64 * 248.0, 28.0)).collect()
}

fn block_title(role: Role, info: &InboxAppInfo) -> String {
    match role {
        Role::Source => format!("Watch {0}", info.name),
        Role::Narrow => info.filter_label.to_string(),
        Role::Act => match info.kind {
            super::catalog::InboxKind::Page => "Analyze".into(),
            super::catalog::InboxKind::Message => "Handle the message".into(),
            super::catalog::InboxKind::Ticket => "Start a run".into(),
        },
        Role::Deliver => "What you get".into(),
    }
}

fn block_blurb(role: Role, info: &InboxAppInfo) -> String {
    match role {
        Role::Source => info.work.to_string(),
        Role::Narrow => "Only items that pass this step continue. Disconnect it to watch everything this connection can already see.".into(),
        Role::Act => info.fleet_does.to_string(),
        Role::Deliver => info.outcome.to_string(),
    }
}

pub fn workflow(ws: &str, mut data: PageData) -> Element {
    let info = data.info;
    let roles = pipeline(info.has_filter);
    let n = roles.len();
    let narrow_on = !data.filter().trim().is_empty();
    let exp = experience(info);
    rsx! {
        section { class: "rounded-2xl border {exp.accent} bg-[var(--app-bg-elevated)] p-4",
            div { class: "flex flex-wrap items-baseline justify-between gap-2",
                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                    "{info.name} automation"
                }
                p { class: "m-0 text-xs text-[var(--app-muted)]",
                    "The wires are the path Fleet runs. A dashed wire means that step is not limiting anything."
                }
            }
            div {
                class: "mt-3 flex flex-wrap items-stretch gap-x-2 gap-y-3 rounded-xl border border-dashed border-[var(--app-border)] p-3",
                for (i, role) in roles.iter().copied().enumerate() {
                    {
                        let title = block_title(role, info);
                        let blurb = block_blurb(role, info);
                        let skipped = role == Role::Narrow && !narrow_on;
                        let wire = if i + 1 == n {
                            String::new()
                        } else if skipped {
                            "┄".to_string()
                        } else {
                            "—".to_string()
                        };
                        rsx! {
                            div { key: "{i}", class: "flex items-center gap-2",
                                article {
                                    class: "w-[220px] max-w-full rounded-xl border border-[var(--app-border)] bg-[var(--app-bg)] p-3 shadow-sm",
                                    header {
                                        class: "text-xs font-semibold text-[var(--app-fg)]",
                                        "{title}"
                                    }
                                    p { class: "mt-1 text-[11px] leading-snug text-[var(--app-muted)]", "{blurb}" }
                                    { block_body(ws, data, role) }
                                }
                                if !wire.is_empty() {
                                    span { class: "text-[var(--app-muted)]", "{wire}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn block_body(ws: &str, mut data: PageData, role: Role) -> Element {
    let info = data.info;
    match role {
        Role::Source => {
            let on = data.connected();
            let account = (data.account)();
            let label = if !on {
                format!("Not connected · {}", info.name)
            } else if account.is_empty() {
                "Connected".to_string()
            } else {
                format!("Connected · {account}")
            };
            let tone = if on {
                "text-emerald-300"
            } else {
                "text-[var(--app-muted)]"
            };
            rsx! {
                p { class: "mt-2 text-[11px] font-medium {tone}", "{label}" }
            }
        }
        Role::Narrow => narrow_body(ws, data),
        Role::Act => {
            let auto = data.auto_launch();
            let ws_save = ws.to_string();
            rsx! {
                label { class: "mt-2 flex items-start gap-2 text-[11px] text-[var(--app-fg)]",
                    input {
                        r#type: "checkbox",
                        checked: auto,
                        disabled: data.busy(),
                        onchange: move |_| {
                            let next = !auto;
                            data.auto_launch.set(next);
                            save_filter(
                                ws_save.clone(),
                                MonitorPatch {
                                    auto_launch: Some(next),
                                    ..MonitorPatch::default()
                                },
                                data.busy,
                                if next { "Runs start on their own." } else { "You start each run yourself." },
                            );
                        },
                    }
                    span { "Launch automatically when something new matches. Off means you press the button on the item." }
                }
            }
        }
        Role::Deliver => rsx! {},
    }
}

fn narrow_body(ws: &str, mut data: PageData) -> Element {
    let info = data.info;
    if info.id == "github" {
        return github_repos(ws, data);
    }
    let ws_save = ws.to_string();
    let multiline = info.id == "jira";
    rsx! {
        if multiline {
            textarea {
                class: "mt-2 w-full rounded-lg border border-[var(--app-border)] bg-transparent px-2 py-1 text-[11px] text-[var(--app-fg)]",
                rows: "3",
                value: "{data.filter()}",
                placeholder: "{info.filter_placeholder}",
                oninput: move |evt| data.filter.set(evt.value()),
            }
        } else {
            input {
                class: "mt-2 w-full rounded-lg border border-[var(--app-border)] bg-transparent px-2 py-1 text-[11px] text-[var(--app-fg)]",
                value: "{data.filter()}",
                placeholder: "{info.filter_placeholder}",
                oninput: move |evt| data.filter.set(evt.value()),
            }
        }
        button {
            class: "mt-2 rounded-lg border border-[var(--app-border)] px-2 py-1 text-[11px] text-[var(--app-fg)]",
            r#type: "button",
            disabled: data.busy(),
            onclick: move |_| {
                save_filter(
                    ws_save.clone(),
                    patch_for(info.id, data.filter()),
                    data.busy,
                    "Saved.",
                );
            },
            "{info.filter_save}"
        }
    }
}

fn github_repos(ws: &str, mut data: PageData) -> Element {
    let allowed = data.allowed();
    let stored = csv_list(data.filter());
    let ws_save = ws.to_string();
    if !data.connected() {
        return rsx! {
            p { class: "mt-2 text-[11px] text-[var(--app-muted)]",
                "Connect GitHub first. The list is the repositories that app is allowed to see."
            }
        };
    }
    if allowed.is_empty() {
        return rsx! {
            p { class: "mt-2 text-[11px] text-[var(--app-muted)]",
                "No repositories yet. Allow them on the GitHub App, then Refresh."
            }
        };
    }
    rsx! {
        ul { class: "mt-2 mb-0 max-h-28 list-none space-y-1 overflow-auto p-0",
            for repo in allowed.clone() {
                {
                    let checked = repo_checked(&stored, &repo);
                    let repo_toggle = repo.clone();
                    let allowed_toggle = allowed.clone();
                    let ws_toggle = ws_save.clone();
                    rsx! {
                        li { key: "{repo}",
                            label { class: "flex items-center gap-2 text-[11px] text-[var(--app-fg)]",
                                input {
                                    r#type: "checkbox",
                                    checked: checked,
                                    disabled: data.busy(),
                                    onchange: move |_| {
                                        let next = toggle_repo(&csv_list(data.filter()), &allowed_toggle, &repo_toggle);
                                        data.filter.set(next.join(", "));
                                        save_filter(
                                            ws_toggle.clone(),
                                            patch_for("github", next.join(", ")),
                                            data.busy,
                                            "Repositories saved.",
                                        );
                                    },
                                }
                                span { class: "break-all", "{repo}" }
                            }
                        }
                    }
                }
            }
        }
        p { class: "mt-1 text-[10px] text-[var(--app-muted)]",
            "All checked means every repo the app can see."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apps_without_a_filter_skip_the_narrow_block() {
        assert_eq!(
            pipeline(false),
            vec![Role::Source, Role::Act, Role::Deliver]
        );
        assert_eq!(
            pipeline(true),
            vec![Role::Source, Role::Narrow, Role::Act, Role::Deliver]
        );
    }

    #[test]
    fn unchecking_one_repo_keeps_the_others() {
        let allowed = vec!["a/one".into(), "a/two".into(), "a/three".into()];
        let next = toggle_repo(&[], &allowed, "a/two");
        assert_eq!(next, vec!["a/one".to_string(), "a/three".to_string()]);
        assert!(!repo_checked(&next, "a/two"));
        assert!(repo_checked(&next, "a/one"));
    }

    #[test]
    fn checking_the_last_gap_means_every_repo() {
        let allowed = vec!["a/one".into(), "a/two".into()];
        let next = toggle_repo(&["a/one".into()], &allowed, "a/two");
        assert!(next.is_empty());
        assert!(repo_checked(&next, "a/one"));
        assert!(repo_checked(&next, "a/two"));
    }

    #[test]
    fn unknown_repo_is_ignored() {
        let allowed = vec!["a/one".into()];
        assert!(toggle_repo(&[], &allowed, "other/repo").is_empty());
    }
}
