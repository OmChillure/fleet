//! Workspace-scoped fleet control plane UI (cookie auth).

use dioxus::prelude::*;
use serde_json::Value;

use super::api::{
    cancel_run, ensure_fleet, list_runs, list_tasks, run_tick, start_fixture,
};

#[derive(Clone, PartialEq)]
struct TaskCard {
    id: String,
    plan_task_id: String,
    title: String,
    status: String,
    priority: i64,
    attempt: i64,
}

#[derive(Clone, PartialEq)]
struct RunCard {
    id: String,
    status: String,
    source: String,
}

fn parse_tasks(v: &Value) -> Vec<TaskCard> {
    v["tasks"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    Some(TaskCard {
                        id: t["id"].as_str()?.to_string(),
                        plan_task_id: t["plan_task_id"].as_str().unwrap_or("?").to_string(),
                        title: t["title"].as_str().unwrap_or("Untitled").to_string(),
                        status: t["status"].as_str().unwrap_or("?").to_string(),
                        priority: t["priority"].as_i64().unwrap_or(0),
                        attempt: t["attempt"].as_i64().unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_runs(v: &Value) -> Vec<RunCard> {
    v["runs"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|r| {
                    Some(RunCard {
                        id: r["id"].as_str()?.to_string(),
                        status: r["status"].as_str().unwrap_or("?").to_string(),
                        source: r["source"].as_str().unwrap_or("").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn status_column(status: &str) -> &'static str {
    match status {
        "queued" | "ready" => "Ready",
        "assigned" | "in_progress" => "In progress",
        "blocked" | "retry_wait" => "Blocked",
        "done_success" => "Done",
        "done_failed" | "cancelled" => "Failed",
        _ => "Other",
    }
}

fn status_pill_class(status: &str) -> &'static str {
    match status {
        "done_success" => "bg-emerald-500/15 text-emerald-400 border-emerald-500/30",
        "done_failed" | "cancelled" => "bg-red-500/15 text-red-400 border-red-500/30",
        "in_progress" | "assigned" => "bg-sky-500/15 text-sky-400 border-sky-500/30",
        "blocked" | "retry_wait" => "bg-amber-500/15 text-amber-400 border-amber-500/30",
        "ready" | "queued" => "bg-[var(--app-bg-hover)] text-[var(--app-muted)] border-[var(--app-border)]",
        _ => "bg-[var(--app-bg-hover)] text-[var(--app-muted)] border-[var(--app-border)]",
    }
}

#[component]
pub fn Fleet(workspace_id: String) -> Element {
    let mut status_line = use_signal(|| "Loading workspace fleet…".to_string());
    let mut project_name = use_signal(|| String::new());
    let mut tasks = use_signal(|| Vec::<TaskCard>::new());
    let mut runs = use_signal(|| Vec::<RunCard>::new());
    let mut last_tick = use_signal(|| String::new());
    let mut busy = use_signal(|| false);
    let mut ready = use_signal(|| false);

    let ws = workspace_id.clone();
    use_effect(move || {
        let ws = ws.clone();
        spawn(async move {
            match ensure_fleet(&ws).await {
                Ok(v) => {
                    project_name.set(
                        v["name"]
                            .as_str()
                            .unwrap_or("Workspace fleet")
                            .to_string(),
                    );
                    status_line.set(format!(
                        "Fleet ready · project {}",
                        v["project_id"].as_str().unwrap_or(&ws)
                    ));
                    ready.set(true);
                    if let Ok(t) = list_tasks(&ws).await {
                        tasks.set(parse_tasks(&t));
                    }
                    if let Ok(r) = list_runs(&ws).await {
                        runs.set(parse_runs(&r));
                    }
                }
                Err(e) => {
                    status_line.set(e.message());
                    ready.set(false);
                }
            }
        });
    });

    let columns = ["Ready", "In progress", "Blocked", "Done", "Failed"];

    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-6xl flex-col gap-4 overflow-auto p-4 md:p-5",
            div { class: "flex flex-wrap items-start justify-between gap-3",
                div {
                    h1 { class: "m-0 text-2xl font-bold tracking-tight text-[var(--app-fg)]", "Fleet" }
                    p { class: "mt-1 text-sm text-[var(--app-muted)]",
                        if project_name().is_empty() {
                            "Workspace-scoped orchestrator project"
                        } else {
                            "{project_name}"
                        }
                    }
                    p { class: "mt-0.5 font-mono text-xs text-[var(--app-muted)]", "workspace {workspace_id}" }
                }
                div { class: "flex flex-wrap gap-2",
                    button {
                        class: "rounded-md bg-[var(--app-accent)] px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50",
                        disabled: busy() || !ready(),
                        r#type: "button",
                        onclick: {
                            let workspace_id = workspace_id.clone();
                            move |_| {
                                busy.set(true);
                                let ws = workspace_id.clone();
                                spawn(async move {
                                    match start_fixture(&ws).await {
                                        Ok(v) => {
                                            status_line.set(format!(
                                                "Started run {}",
                                                v["run_id"].as_str().unwrap_or("?")
                                            ));
                                        }
                                        Err(e) => status_line.set(e.message()),
                                    }
                                    if let Ok(t) = list_tasks(&ws).await {
                                        tasks.set(parse_tasks(&t));
                                    }
                                    if let Ok(r) = list_runs(&ws).await {
                                        runs.set(parse_runs(&r));
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                        "Start 2-task run"
                    }
                    button {
                        class: "rounded-md bg-emerald-600 px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50",
                        disabled: busy() || !ready(),
                        r#type: "button",
                        onclick: {
                            let workspace_id = workspace_id.clone();
                            move |_| {
                                busy.set(true);
                                let ws = workspace_id.clone();
                                spawn(async move {
                                    match run_tick(&ws).await {
                                        Ok(v) => {
                                            last_tick.set(
                                                serde_json::to_string_pretty(&v).unwrap_or_default(),
                                            );
                                            status_line.set(format!(
                                                "Tick · spawned {} · completed {} · runs ended {}",
                                                v["spawned"],
                                                v["completed_via_runtime"],
                                                v["runs_terminated"]
                                            ));
                                        }
                                        Err(e) => status_line.set(e.message()),
                                    }
                                    if let Ok(t) = list_tasks(&ws).await {
                                        tasks.set(parse_tasks(&t));
                                    }
                                    if let Ok(r) = list_runs(&ws).await {
                                        runs.set(parse_runs(&r));
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                        "Run tick"
                    }
                    button {
                        class: "rounded-md border border-[var(--app-border)] px-3 py-1.5 text-sm font-medium text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)] disabled:opacity-50",
                        disabled: busy() || !ready(),
                        r#type: "button",
                        onclick: {
                            let workspace_id = workspace_id.clone();
                            move |_| {
                                busy.set(true);
                                let ws = workspace_id.clone();
                                spawn(async move {
                                    match list_tasks(&ws).await {
                                        Ok(t) => {
                                            tasks.set(parse_tasks(&t));
                                            status_line.set("Board refreshed.".into());
                                        }
                                        Err(e) => status_line.set(e.message()),
                                    }
                                    if let Ok(r) = list_runs(&ws).await {
                                        runs.set(parse_runs(&r));
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                        "Refresh"
                    }
                    button {
                        class: "rounded-md border border-red-500/40 px-3 py-1.5 text-sm font-medium text-red-400 hover:bg-red-500/10 disabled:opacity-50",
                        disabled: busy() || !ready() || runs().is_empty(),
                        r#type: "button",
                        onclick: {
                            let workspace_id = workspace_id.clone();
                            move |_| {
                                let Some(run) = runs().into_iter().next() else { return };
                                busy.set(true);
                                let ws = workspace_id.clone();
                                spawn(async move {
                                    match cancel_run(&ws, &run.id).await {
                                        Ok(_) => status_line.set(format!("Cancel requested for {}", run.id)),
                                        Err(e) => status_line.set(e.message()),
                                    }
                                    if let Ok(r) = list_runs(&ws).await {
                                        runs.set(parse_runs(&r));
                                    }
                                    if let Ok(t) = list_tasks(&ws).await {
                                        tasks.set(parse_tasks(&t));
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                        "Cancel latest"
                    }
                }
            }

            p { class: "text-sm text-[var(--app-fg)]", "{status_line}" }

            // Runs strip
            section { class: "rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 mb-2 text-sm font-semibold uppercase tracking-wide text-[var(--app-muted)]", "Runs" }
                if runs().is_empty() {
                    p { class: "text-sm text-[var(--app-muted)]", "No runs yet. Start a 2-task run." }
                } else {
                    div { class: "flex flex-col gap-2",
                        for run in runs() {
                            div {
                                key: "{run.id}",
                                class: "flex flex-wrap items-center justify-between gap-2 rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2",
                                div { class: "min-w-0",
                                    p { class: "m-0 font-mono text-xs text-[var(--app-muted)]", "{run.id}" }
                                    p { class: "m-0 text-sm text-[var(--app-fg)]", "source: {run.source}" }
                                }
                                span {
                                    class: "rounded-full border px-2 py-0.5 text-xs font-medium {status_pill_class(&run.status)}",
                                    "{run.status}"
                                }
                            }
                        }
                    }
                }
            }

            // Kanban-ish board
            section {
                h2 { class: "m-0 mb-2 text-sm font-semibold uppercase tracking-wide text-[var(--app-muted)]", "Board" }
                div { class: "grid gap-3 md:grid-cols-5",
                    for col in columns {
                        {
                            let col_tasks: Vec<TaskCard> = tasks()
                                .into_iter()
                                .filter(|t| status_column(&t.status) == col)
                                .collect();
                            rsx! {
                                div {
                                    key: "{col}",
                                    class: "flex min-h-48 flex-col rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-2",
                                    h3 { class: "m-0 mb-2 px-1 text-xs font-semibold uppercase tracking-wide text-[var(--app-muted)]",
                                        "{col} ({col_tasks.len()})"
                                    }
                                    div { class: "flex flex-1 flex-col gap-2",
                                        if col_tasks.is_empty() {
                                            p { class: "px-1 text-xs text-[var(--app-muted)]", "—" }
                                        }
                                        for t in col_tasks {
                                            div {
                                                key: "{t.id}",
                                                class: "rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] p-2.5 shadow-sm",
                                                p { class: "m-0 text-sm font-medium text-[var(--app-fg)]", "{t.title}" }
                                                p { class: "m-0 mt-0.5 font-mono text-[10px] text-[var(--app-muted)]", "{t.plan_task_id}" }
                                                div { class: "mt-2 flex flex-wrap items-center gap-1.5",
                                                    span {
                                                        class: "rounded-full border px-1.5 py-0.5 text-[10px] font-medium {status_pill_class(&t.status)}",
                                                        "{t.status}"
                                                    }
                                                    span { class: "text-[10px] text-[var(--app-muted)]", "p{t.priority}" }
                                                    span { class: "text-[10px] text-[var(--app-muted)]", "try {t.attempt}" }
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

            if !last_tick().is_empty() {
                details { class: "rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-3",
                    summary { class: "cursor-pointer text-xs font-semibold uppercase tracking-wide text-[var(--app-muted)]", "Last tick JSON" }
                    pre { class: "mt-2 max-h-48 overflow-auto whitespace-pre-wrap break-all font-mono text-xs text-[var(--app-muted)]",
                        "{last_tick}"
                    }
                }
            }

            p { class: "text-xs text-[var(--app-muted)]",
                "Uses your login session (cookies). Project id = workspace id. "
                "After starting a run, press "
                strong { "Run tick" }
                " a few times until the board reaches Done."
            }
        }
    }
}
