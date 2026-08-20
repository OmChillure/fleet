use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde_json::Value;

use super::api::{Overview, fetch_overview};
use crate::Route;

fn short(id: &str) -> String {
    id.get(..8).unwrap_or(id).to_string()
}

#[derive(Clone, PartialEq)]
struct TaskRow {
    id: String,
    title: String,
    plan_id: String,
    profile: String,
    status: String,
}

#[derive(Clone, PartialEq)]
struct ChatRow {
    id: String,
    kind: String,
    from: String,
    to: String,
    body: String,
}

fn parse_tasks(v: &[Value]) -> Vec<TaskRow> {
    v.iter()
        .filter_map(|t| {
            Some(TaskRow {
                id: t.get("id")?.as_str()?.to_string(),
                title: t
                    .get("title")
                    .and_then(|x| x.as_str())
                    .unwrap_or("task")
                    .to_string(),
                plan_id: t
                    .get("plan_task_id")
                    .and_then(|x| x.as_str())
                    .unwrap_or("?")
                    .to_string(),
                profile: t
                    .get("worker_profile")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                status: t
                    .get("status")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect()
}

fn parse_chat(v: &[Value]) -> Vec<ChatRow> {
    v.iter()
        .filter_map(|m| {
            Some(ChatRow {
                id: m.get("id")?.as_str()?.to_string(),
                kind: m
                    .get("kind")
                    .and_then(|x| x.as_str())
                    .unwrap_or("say")
                    .to_string(),
                from: m
                    .get("from")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                to: m
                    .get("to")
                    .and_then(|x| x.as_str())
                    .unwrap_or("*")
                    .to_string(),
                body: m
                    .get("body")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect()
}

fn status_class(status: &str) -> &'static str {
    match status {
        "done_success" | "completed" => "text-emerald-400",
        "in_progress" | "assigned" | "healthy" | "spawning" | "active" => "text-sky-400",
        "queued" | "ready" | "retry_wait" => "text-amber-400",
        "done_failed" | "failed" | "failed_spawn" => "text-red-400",
        _ => "text-[var(--app-muted)]",
    }
}

#[component]
pub fn Dashboard(workspace_id: String) -> Element {
    let mut data = use_signal(Overview::default);
    let mut error = use_signal(|| None::<String>);
    let mut loaded = use_signal(|| false);
    let ws = workspace_id.clone();

    use_effect(move || {
        let ws = ws.clone();
        spawn(async move {
            loop {
                match fetch_overview(&ws).await {
                    Ok(o) => {
                        data.set(o);
                        error.set(None);
                    }
                    Err(e) => error.set(Some(e.message())),
                }
                loaded.set(true);
                TimeoutFuture::new(2000).await;
            }
        });
    });

    let o = data();
    let tasks = parse_tasks(&o.tasks);
    let waiting = parse_chat(&o.waiting);
    let chat = parse_chat(&o.chat);
    let tickets: Vec<(String, String, String, String)> = o
        .tickets
        .iter()
        .filter_map(|t| {
            Some((
                t.get("provider")?.as_str()?.to_string(),
                t.get("key")?.as_str()?.to_string(),
                t.get("title")?.as_str()?.to_string(),
                t.get("state")?.as_str()?.to_string(),
            ))
        })
        .collect();
    let activity: Vec<(String, String, String, String)> = o
        .activity
        .iter()
        .filter_map(|a| {
            Some((
                a.get("profile")?.as_str()?.to_string(),
                a.get("kind")?.as_str()?.to_string(),
                a.get("text")?.as_str()?.to_string(),
                a.get("status")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
            ))
        })
        .collect();

    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-6xl flex-col gap-4 overflow-auto p-4 md:p-5",
            div {
                h1 { class: "m-0 text-2xl font-bold tracking-tight text-[var(--app-fg)]",
                    "Dashboard"
                }
                p { class: "mt-1 text-sm text-[var(--app-muted)]",
                    "Live team: who is working, what they're thinking, who asked whom."
                }
            }

            if let Some(err) = error() {
                p { class: "text-sm text-red-400", "{err}" }
            } else if !loaded() {
                p { class: "text-sm text-[var(--app-muted)]", "Loading fleet overview…" }
            }

            div { class: "grid gap-3 sm:grid-cols-2 lg:grid-cols-4",
                Stat { label: "Open runs", value: "{o.open_runs}" }
                Stat { label: "Live agents", value: "{o.live_agents}" }
                Stat { label: "In progress", value: "{o.tasks_in_progress}" }
                Stat { label: "Sent back", value: "{o.changes_requested}" }
            }

            section { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Tickets" }
                p { class: "mt-1 text-xs text-[var(--app-muted)]",
                    "Chat: @fleet monitor to watch GitHub / Jira / Linear. @fleet finish KEY to run a fleet."
                }
                if tickets.is_empty() {
                    p { class: "mt-3 text-sm text-[var(--app-muted)]",
                        "No inbox yet. Connect a board and say @fleet monitor."
                    }
                } else {
                    ul { class: "mt-3 m-0 list-none space-y-2 p-0",
                        for (provider, key, title, st) in tickets {
                            li {
                                key: "{provider}-{key}",
                                class: "flex items-start justify-between gap-2",
                                div {
                                    p { class: "m-0 text-sm text-[var(--app-fg)]",
                                        "{title}"
                                    }
                                    p { class: "m-0 font-mono text-[11px] text-[var(--app-muted)]",
                                        "{provider} · {key}"
                                    }
                                }
                                span {
                                    class: "shrink-0 text-xs {status_class(&st)}",
                                    "{st}"
                                }
                            }
                        }
                    }
                }
            }

            div { class: "grid gap-4 lg:grid-cols-2",
                section { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                    h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Team board" }
                    if let Some(rid) = o.focus_run_id.clone() {
                        p { class: "mt-1 font-mono text-[11px] text-[var(--app-muted)]",
                            "run {short(&rid)}"
                        }
                    }
                    if tasks.is_empty() {
                        p { class: "mt-3 text-sm text-[var(--app-muted)]",
                            "No runs yet. Start one from Chat or Fleet."
                        }
                    } else {
                        ul { class: "mt-3 m-0 list-none space-y-2 p-0",
                            for t in tasks {
                                li {
                                    key: "{t.id}",
                                    class: "flex items-start justify-between gap-2",
                                    div {
                                        p { class: "m-0 text-sm text-[var(--app-fg)]",
                                            "{t.title}"
                                        }
                                        p { class: "m-0 font-mono text-[11px] text-[var(--app-muted)]",
                                            "{t.plan_id} · {t.profile}"
                                        }
                                    }
                                    span {
                                        class: "shrink-0 text-xs {status_class(&t.status)}",
                                        "{t.status}"
                                    }
                                }
                            }
                        }
                    }
                    Link {
                        to: Route::Fleet { workspace_id: workspace_id.clone() },
                        class: "mt-3 inline-block text-xs text-[var(--app-muted)] underline",
                        "Open Fleet →"
                    }
                }

                section { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                    h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Who asked whom" }
                    if waiting.is_empty() {
                        p { class: "mt-3 text-sm text-[var(--app-muted)]",
                            "No open questions. Agents ask each other when they are blocked."
                        }
                    } else {
                        ul { class: "mt-3 m-0 list-none space-y-3 p-0",
                            for w in waiting {
                                li {
                                    key: "{w.id}",
                                    p { class: "m-0 text-xs text-[var(--app-muted)]",
                                        "`{w.from}` → `{w.to}`"
                                    }
                                    p { class: "m-0 mt-1 text-sm text-[var(--app-fg)]",
                                        "{w.body}"
                                    }
                                }
                            }
                        }
                    }
                }
            }

            section { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "What agents are doing" }
                if activity.is_empty() {
                    p { class: "mt-3 text-sm text-[var(--app-muted)]",
                        "Thinking, tools, and teammate messages show up here as they work. Open a session for the full log."
                    }
                } else {
                    ul { class: "mt-3 m-0 list-none space-y-2 p-0",
                        for (profile, kind, text, st) in activity {
                            li {
                                key: "{profile}-{kind}-{text}",
                                p { class: "m-0 text-[11px] text-[var(--app-muted)]",
                                    "`{profile}` · {kind} · {st}"
                                }
                                p { class: "m-0 mt-0.5 whitespace-pre-wrap text-sm text-[var(--app-fg)]",
                                    "{text}"
                                }
                            }
                        }
                    }
                }
            }

            section { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Team chat" }
                if chat.is_empty() {
                    p { class: "mt-3 text-sm text-[var(--app-muted)]",
                        "When specialists message each other, it shows up here."
                    }
                } else {
                    ul { class: "mt-3 m-0 list-none space-y-2 p-0",
                        for m in chat {
                            li {
                                key: "{m.id}",
                                p { class: "m-0 text-[11px] text-[var(--app-muted)]",
                                    "{m.kind} · `{m.from}` → `{m.to}`"
                                }
                                p { class: "m-0 mt-0.5 whitespace-pre-wrap text-sm text-[var(--app-fg)]",
                                    "{m.body}"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn Stat(label: &'static str, value: String) -> Element {
    rsx! {
        div { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
            p { class: "m-0 text-[11px] uppercase tracking-wide text-[var(--app-muted)]", "{label}" }
            p { class: "m-0 mt-1 text-2xl font-semibold text-[var(--app-fg)]", "{value}" }
        }
    }
}
