use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use serde_json::Value;

use super::api::{
    Overview, WorkforceSnapshot, create_goal, decide_approval, fetch_overview, fetch_workforce,
    pull_github_issues,
};
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
    session_id: String,
}

#[derive(Clone, PartialEq)]
struct ChatRow {
    id: String,
    kind: String,
    from: String,
    to: String,
    body: String,
}

#[derive(Clone, PartialEq)]
struct ActivityRow {
    key: String,
    profile: String,
    kind: String,
    text: String,
    status: String,
    session_id: String,
}

/// Collapse token-sized `update` fragments from the same session so the
/// dashboard list stays readable and Dioxus keys stay unique.
fn parse_activity(items: &[Value]) -> Vec<ActivityRow> {
    let mut out: Vec<ActivityRow> = Vec::new();
    for (i, a) in items.iter().enumerate() {
        let Some(profile) = a.get("profile").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(kind) = a.get("kind").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(text) = a.get("text").and_then(|v| v.as_str()) else {
            continue;
        };
        let status = a
            .get("status")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let session_id = a
            .get("session_id")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        if let Some(last) = out.last_mut() {
            if last.session_id == session_id && last.kind == "update" && kind == "update" {
                last.text.push_str(text);
                continue;
            }
        }
        let seq = a
            .get("report_seq")
            .and_then(|v| v.as_i64())
            .unwrap_or(i as i64);
        let idx = a.get("idx").and_then(|v| v.as_i64()).unwrap_or(0);
        out.push(ActivityRow {
            key: format!("{session_id}-{seq}-{idx}-{i}"),
            profile: profile.to_string(),
            kind: kind.to_string(),
            text: text.to_string(),
            status,
            session_id,
        });
    }
    out
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
                session_id: t
                    .get("claimed_session_id")
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

fn decide_from_dash(
    workspace_id: String,
    approval_id: String,
    decision: &'static str,
    mut busy: Signal<bool>,
    mut status_line: Signal<String>,
    mut workforce: Signal<WorkforceSnapshot>,
) {
    if busy() {
        return;
    }
    busy.set(true);
    spawn(async move {
        match decide_approval(&workspace_id, &approval_id, decision).await {
            Ok(_) => status_line.set(format!("Decision recorded: {decision}.")),
            Err(e) => status_line.set(e.message()),
        }
        if let Ok(w) = fetch_workforce(&workspace_id).await {
            workforce.set(w);
        }
        busy.set(false);
    });
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
    let mut workforce = use_signal(WorkforceSnapshot::default);
    let mut error = use_signal(|| None::<String>);
    let mut loaded = use_signal(|| false);
    let mut goal_title = use_signal(String::new);
    let mut goal_md = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut status_line = use_signal(String::new);
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
                if let Ok(w) = fetch_workforce(&ws).await {
                    workforce.set(w);
                }
                loaded.set(true);
                TimeoutFuture::new(2000).await;
            }
        }
    });

    let o = data();
    let wf = workforce();
    let tasks = parse_tasks(&o.tasks);
    let waiting = parse_chat(&o.waiting);
    let chat = parse_chat(&o.chat);

    let activity = parse_activity(&o.activity);

    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-6xl flex-col gap-4 overflow-auto p-4 md:p-5",
            div {
                h1 { class: "m-0 text-2xl font-bold tracking-tight text-[var(--app-fg)]",
                    "Workforce"
                }
                p { class: "mt-1 text-sm text-[var(--app-muted)]",
                    "Work arrives from GitHub. The crew commits and opens the pull request. A person merges, sends the message, or deploys."
                }
            }

            section { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                div { class: "flex flex-wrap items-center justify-between gap-2",
                    h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Arriving work" }
                    button {
                        class: "rounded-md border border-[var(--app-border)] px-3 py-1.5 text-sm font-medium text-[var(--app-fg)] disabled:opacity-50",
                        r#type: "button",
                        disabled: busy(),
                        onclick: {
                            let workspace_id = workspace_id.clone();
                            move |_| {
                                if busy() { return; }
                                busy.set(true);
                                let ws = workspace_id.clone();
                                spawn(async move {
                                    match pull_github_issues(&ws).await {
                                        Ok(v) => {
                                            let n = v.get("accepted").and_then(|x| x.as_array()).map(|a| a.len()).unwrap_or(0);
                                            let refused = v.get("refused").and_then(|x| x.as_array()).map(|a| a.len()).unwrap_or(0);
                                            status_line.set(format!("Intake accepted {n} issue(s). Refused {refused}."));
                                        }
                                        Err(e) => status_line.set(e.message()),
                                    }
                                    if let Ok(w) = fetch_workforce(&ws).await {
                                        workforce.set(w);
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                        "Pull open GitHub issues"
                    }
                }
                p { class: "mt-2 text-sm text-[var(--app-muted)]",
                    "Vague issues stay out. A specific issue becomes a goal and keeps its source, so the next pull does not open it twice."
                }
            }

            if !wf.pending_approvals.is_empty() {
                section { class: "rounded-2xl border border-amber-500/40 bg-[var(--app-bg-elevated)] p-4",
                    h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Waiting for a person" }
                    ul { class: "mt-3 m-0 list-none space-y-3 p-0",
                        for a in wf.pending_approvals.clone() {
                            {
                                let aid = a.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let summary = a.get("summary").and_then(|v| v.as_str()).unwrap_or("approval").to_string();
                                let kind = a.get("kind").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                rsx! {
                                    li {
                                        key: "{aid}",
                                        class: "flex flex-wrap items-center justify-between gap-2",
                                        div {
                                            p { class: "m-0 text-sm text-[var(--app-fg)]", "{summary}" }
                                            p { class: "m-0 text-xs text-[var(--app-muted)]", "{kind}" }
                                        }
                                        div { class: "flex gap-2",
                                            button {
                                                class: "rounded-md bg-[var(--app-accent)] px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50",
                                                r#type: "button",
                                                disabled: busy() || aid.is_empty(),
                                                onclick: {
                                                    let workspace_id = workspace_id.clone();
                                                    let aid = aid.clone();
                                                    move |_| decide_from_dash(workspace_id.clone(), aid.clone(), "approve", busy, status_line, workforce)
                                                },
                                                "Approve"
                                            }
                                            button {
                                                class: "rounded-md border border-[var(--app-border)] px-3 py-1.5 text-sm text-[var(--app-fg)] disabled:opacity-50",
                                                r#type: "button",
                                                disabled: busy() || aid.is_empty(),
                                                onclick: {
                                                    let workspace_id = workspace_id.clone();
                                                    let aid = aid.clone();
                                                    move |_| decide_from_dash(workspace_id.clone(), aid.clone(), "deny", busy, status_line, workforce)
                                                },
                                                "Deny"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            section { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Set a goal" }
                input {
                    class: "mt-3 w-full rounded-md border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                    r#type: "text",
                    placeholder: "Ship the weather app from #eng",
                    value: "{goal_title}",
                    oninput: move |e| goal_title.set(e.value()),
                }
                textarea {
                    class: "mt-2 w-full rounded-md border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                    rows: "3",
                    placeholder: "Done when the ticket is closed and the PR is merged.",
                    value: "{goal_md}",
                    oninput: move |e| goal_md.set(e.value()),
                }
                button {
                    class: "mt-2 rounded-md bg-[var(--app-accent)] px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50",
                    r#type: "button",
                    disabled: busy() || goal_title().trim().is_empty(),
                    onclick: {
                        let workspace_id = workspace_id.clone();
                        move |_| {
                            if busy() { return; }
                            busy.set(true);
                            let ws = workspace_id.clone();
                            let title = goal_title();
                            let md = goal_md();
                            spawn(async move {
                                match create_goal(&ws, title.trim(), md.trim()).await {
                                    Ok(_) => {
                                        goal_title.set(String::new());
                                        goal_md.set(String::new());
                                        status_line.set("Goal is on the board. It stays until the outcome is done.".into());
                                    }
                                    Err(e) => status_line.set(e.message()),
                                }
                                if let Ok(w) = fetch_workforce(&ws).await {
                                    workforce.set(w);
                                }
                                busy.set(false);
                            });
                        }
                    },
                    "Start goal"
                }
                if !status_line().is_empty() {
                    p { class: "mt-2 text-sm text-[var(--app-muted)]", "{status_line}" }
                }
            }

            if let Some(err) = error() {
                p { class: "text-sm text-red-400", "{err}" }
            } else if !loaded() {
                p { class: "text-sm text-[var(--app-muted)]", "Loading fleet overview…" }
            }

            div { class: "grid gap-3 sm:grid-cols-2 lg:grid-cols-4",
                Stat { label: "Open goals", value: "{wf.open_goals.len()}" }
                Stat { label: "Open runs", value: "{o.open_runs}" }
                Stat { label: "Live agents", value: "{o.live_agents}" }
                Stat { label: "In progress", value: "{o.tasks_in_progress}" }
            }

            section { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Goals" }
                if wf.goals.is_empty() {
                    p { class: "mt-3 text-sm text-[var(--app-muted)]",
                        "No goals yet. Set one above or say it in Chat — it stays on this board until the ticket or PR is done."
                    }
                } else {
                    ul { class: "mt-3 m-0 list-none space-y-2 p-0",
                        for g in wf.goals.clone() {
                            {
                                let gid = g.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let title = g.get("title").and_then(|v| v.as_str()).unwrap_or("goal").to_string();
                                let status = g.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let run_id = g.get("run_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                let sub = if run_id.is_empty() {
                                    "case-backed".to_string()
                                } else {
                                    format!("run {}", short(&run_id))
                                };
                                rsx! {
                                    li {
                                        key: "{gid}",
                                        class: "flex items-start justify-between gap-2",
                                        div {
                                            p { class: "m-0 text-sm text-[var(--app-fg)]", "{title}" }
                                            p { class: "m-0 font-mono text-[11px] text-[var(--app-muted)]",
                                                "{sub}"
                                            }
                                        }
                                        span { class: "shrink-0 text-xs {status_class(&status)}", "{status}" }
                                    }
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
                                    div { class: "flex shrink-0 items-center gap-2",
                                        span {
                                            class: "text-xs {status_class(&t.status)}",
                                            "{t.status}"
                                        }
                                        if !t.session_id.is_empty() {
                                            Link {
                                                to: Route::SessionDetail {
                                                    workspace_id: workspace_id.clone(),
                                                    session_id: t.session_id.clone(),
                                                },
                                                class: "text-xs text-[var(--app-accent)] underline",
                                                "Watch"
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
                        for row in activity {
                            li {
                                key: "{row.key}",
                                p { class: "m-0 text-[11px] text-[var(--app-muted)]",
                                    "`{row.profile}` · {row.kind} · {row.status}"
                                }
                                p { class: "m-0 mt-0.5 whitespace-pre-wrap text-sm text-[var(--app-fg)]",
                                    "{row.text}"
                                }
                                if !row.session_id.is_empty() {
                                    Link {
                                        to: Route::SessionDetail {
                                            workspace_id: workspace_id.clone(),
                                            session_id: row.session_id.clone(),
                                        },
                                        class: "mt-1 inline-block text-xs text-[var(--app-accent)] underline",
                                        "Watch session"
                                    }
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
