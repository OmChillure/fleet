use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde_json::Value;

use super::api::{Overview, fetch_overview};
use crate::pages::fleet::api::resume_task;
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
    block_reason: String,
    hitl_notes: String,
    attempt: i64,
    worker_state: String,
    run_id: String,
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
                block_reason: t
                    .get("block_reason")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                hitl_notes: t
                    .get("hitl_notes")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                attempt: t.get("attempt").and_then(|x| x.as_i64()).unwrap_or(0),
                worker_state: t
                    .get("worker_state")
                    .and_then(|x| x.as_str())
                    .unwrap_or("none")
                    .to_string(),
                run_id: t
                    .get("run_id")
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
        "blocked" | "retry_wait" => "text-amber-400",
        "queued" | "ready" => "text-[var(--app-muted)]",
        "done_failed" | "failed" | "failed_spawn" => "text-red-400",
        _ => "text-[var(--app-muted)]",
    }
}

fn board_column(status: &str) -> &'static str {
    match status {
        "queued" | "ready" => "Ready",
        "assigned" | "in_progress" => "In progress",
        "blocked" | "retry_wait" => "Blocked",
        "done_success" | "completed" => "Done",
        "done_failed" | "cancelled" | "failed" => "Failed",
        _ => "Ready",
    }
}

fn worker_label(state: &str) -> &'static str {
    match state {
        "live" => "live",
        "reclaimed" => "reclaimed",
        "dead" => "dead",
        "ended" => "ended",
        _ => "idle",
    }
}

#[component]
pub fn Dashboard(workspace_id: String) -> Element {
    let mut data = use_signal(Overview::default);
    let mut error = use_signal(|| None::<String>);
    let mut loaded = use_signal(|| false);
    let mut resume_note = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let ws = workspace_id.clone();

    use_future(move || {
        let ws = ws.clone();
        async move {
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
        }
    });

    let o = data();
    let tasks = parse_tasks(&o.tasks);
    let waiting = parse_chat(&o.waiting);
    let chat = parse_chat(&o.chat);

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
                    "Workforce"
                }
                p { class: "mt-1 text-sm text-[var(--app-muted)]",
                    "The team board. Chat starts work; this page keeps it — who is on what, what is blocked, who was reclaimed after a dead worker. Same board tomorrow."
                }
            }

            if let Some(err) = error() {
                p { class: "text-sm text-red-400", "{err}" }
            } else if !loaded() {
                p { class: "text-sm text-[var(--app-muted)]", "Loading workforce…" }
            }

            div { class: "grid gap-3 sm:grid-cols-2 lg:grid-cols-5",
                Stat { label: "Open runs", value: "{o.open_runs}" }
                Stat { label: "Live agents", value: "{o.live_agents}" }
                Stat { label: "In progress", value: "{o.tasks_in_progress}" }
                Stat { label: "Blocked", value: "{o.tasks_blocked}" }
                Stat { label: "Sent back", value: "{o.changes_requested}" }
            }

            section { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Team board" }
                p { class: "mt-1 text-[11px] text-[var(--app-muted)]",
                    "All open runs. A dead worker is retried on the next tick — the card stays here."
                }
                if tasks.is_empty() {
                    p { class: "mt-3 text-sm text-[var(--app-muted)]",
                        "No work yet. Start a run from Chat or Inbox."
                    }
                } else {
                    div { class: "mt-3 grid gap-2 md:grid-cols-5",
                        for col in ["Ready", "In progress", "Blocked", "Done", "Failed"] {
                            {
                                let col_tasks: Vec<TaskRow> = tasks
                                    .iter()
                                    .filter(|t| board_column(&t.status) == col)
                                    .cloned()
                                    .collect();
                                rsx! {
                                    div {
                                        key: "{col}",
                                        class: "flex min-h-40 flex-col rounded-xl border border-[var(--app-border)] bg-[var(--app-bg)] p-2",
                                        h3 { class: "m-0 mb-2 px-1 text-[11px] font-semibold uppercase tracking-wide text-[var(--app-muted)]",
                                            "{col} ({col_tasks.len()})"
                                        }
                                        div { class: "flex flex-1 flex-col gap-2",
                                            for t in col_tasks {
                                                div {
                                                    key: "{t.id}",
                                                    class: "rounded-lg border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-2",
                                                    p { class: "m-0 text-sm font-medium text-[var(--app-fg)]",
                                                        "{t.title}"
                                                    }
                                                    p { class: "m-0 mt-0.5 font-mono text-[10px] text-[var(--app-muted)]",
                                                        "{t.plan_id} · {t.profile} · try {t.attempt}"
                                                    }
                                                    p { class: "m-0 mt-0.5 text-[11px] {status_class(&t.status)}",
                                                        "{t.status} · {worker_label(&t.worker_state)}"
                                                        if !t.run_id.is_empty() {
                                                            " · run {short(&t.run_id)}"
                                                        }
                                                    }
                                                    if !t.block_reason.is_empty() {
                                                        p { class: "m-0 mt-1 text-[11px] text-amber-300/90",
                                                            "{t.block_reason}"
                                                        }
                                                    }
                                                    if !t.hitl_notes.is_empty() {
                                                        p { class: "m-0 mt-0.5 text-[11px] text-[var(--app-muted)]",
                                                            "{t.hitl_notes}"
                                                        }
                                                    }
                                                    if t.status == "blocked" {
                                                        input {
                                                            class: "mt-2 w-full rounded-md border border-[var(--app-border)] bg-[var(--app-bg)] px-2 py-1 text-[11px] text-[var(--app-fg)]",
                                                            r#type: "text",
                                                            placeholder: "Resume note",
                                                            value: "{resume_note}",
                                                            oninput: move |e| resume_note.set(e.value()),
                                                        }
                                                        button {
                                                            class: "mt-1 rounded-md bg-amber-600 px-2 py-1 text-[11px] font-medium text-white disabled:opacity-50",
                                                            r#type: "button",
                                                            disabled: busy(),
                                                            onclick: {
                                                                let ws = workspace_id.clone();
                                                                let tid = t.id.clone();
                                                                move |_| {
                                                                    if busy() { return; }
                                                                    busy.set(true);
                                                                    let ws = ws.clone();
                                                                    let tid = tid.clone();
                                                                    let note = resume_note();
                                                                    spawn(async move {
                                                                        let _ = resume_task(&ws, &tid, &note).await;
                                                                        resume_note.set(String::new());
                                                                        busy.set(false);
                                                                    });
                                                                }
                                                            },
                                                            "Resume"
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
                Link {
                    to: Route::Fleet { workspace_id: workspace_id.clone() },
                    class: "mt-3 inline-block text-xs text-[var(--app-muted)] underline",
                    "Operator fleet (ticks, kill) →"
                }
            }

            div { class: "grid gap-4 lg:grid-cols-2",
                section { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                    h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Waiting on a teammate" }
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
