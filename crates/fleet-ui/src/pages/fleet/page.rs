//! Workspace-scoped fleet control plane UI (cookie auth).

use dioxus::prelude::*;
use serde_json::Value;

use super::api::{
    cancel_run, ensure_fleet, list_runs, list_sessions, list_tasks, resume_task,
    run_tick, start_fixture,
};
use crate::Route;
use gloo_timers::future::TimeoutFuture;

#[derive(Clone, PartialEq)]
struct TaskCard {
    id: String,
    plan_task_id: String,
    title: String,
    status: String,
    priority: i64,
    attempt: i64,
    progress_pct: i64,
    worker_profile: String,
    block_reason: String,
    hitl_notes: String,
}

#[derive(Clone, PartialEq)]
struct SessionCard {
    id: String,
    status: String,
    runtime_backend: String,
    progress_pct: i64,
    worker_profile: String,
    work_dir: String,
    exit_reason: String,
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
                        progress_pct: t["progress_pct"].as_i64().unwrap_or(0),
                        worker_profile: t["worker_profile"]
                            .as_str()
                            .unwrap_or("generalist")
                            .to_string(),
                        block_reason: t["block_reason"].as_str().unwrap_or("").to_string(),
                        hitl_notes: t["hitl_notes"].as_str().unwrap_or("").to_string(),
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

fn parse_sessions(v: &Value) -> Vec<SessionCard> {
    v["sessions"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    Some(SessionCard {
                        id: s["id"].as_str()?.to_string(),
                        status: s["status"].as_str().unwrap_or("?").to_string(),
                        runtime_backend: s["runtime_backend"].as_str().unwrap_or("?").to_string(),
                        progress_pct: s["progress_pct"].as_i64().unwrap_or(0),
                        worker_profile: s["worker_profile"].as_str().unwrap_or("").to_string(),
                        work_dir: s["work_dir"].as_str().unwrap_or("").to_string(),
                        exit_reason: s["exit_reason"].as_str().unwrap_or("").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn run_is_terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled")
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
        "done_success" | "completed" | "exited" => {
            "bg-emerald-500/15 text-emerald-400 border-emerald-500/30"
        }
        "done_failed" | "cancelled" | "failed" => "bg-red-500/15 text-red-400 border-red-500/30",
        "in_progress" | "assigned" | "running" | "active" => {
            "bg-sky-500/15 text-sky-400 border-sky-500/30"
        }
        "blocked" | "retry_wait" | "stuck" => "bg-amber-500/15 text-amber-400 border-amber-500/30",
        "ready" | "queued" => {
            "bg-[var(--app-bg-hover)] text-[var(--app-muted)] border-[var(--app-border)]"
        }
        _ => "bg-[var(--app-bg-hover)] text-[var(--app-muted)] border-[var(--app-border)]",
    }
}

#[component]
pub fn Fleet(workspace_id: String) -> Element {
    let mut status_line = use_signal(|| "Loading workspace fleet…".to_string());
    let mut project_name = use_signal(|| String::new());
    let mut sandbox_backend = use_signal(|| "stub".to_string());
    let mut tasks = use_signal(|| Vec::<TaskCard>::new());
    let mut runs = use_signal(|| Vec::<RunCard>::new());
    let mut sessions = use_signal(|| Vec::<SessionCard>::new());
    let mut last_tick = use_signal(|| String::new());
    let mut busy = use_signal(|| false);
    let mut ready = use_signal(|| false);
    let mut resume_note = use_signal(String::new);

    let refresh = {
        let workspace_id = workspace_id.clone();
        move || {
            let ws = workspace_id.clone();
            spawn(async move {
                if let Ok(t) = list_tasks(&ws).await {
                    tasks.set(parse_tasks(&t));
                }
                if let Ok(r) = list_runs(&ws).await {
                    runs.set(parse_runs(&r));
                }
                if let Ok(s) = list_sessions(&ws).await {
                    sessions.set(parse_sessions(&s));
                    if let Some(b) = s["sandbox_backend"].as_str() {
                        sandbox_backend.set(b.to_string());
                    }
                }
            });
        }
    };

    let ws = workspace_id.clone();
    use_effect(move || {
        let ws = ws.clone();
        spawn(async move {
            match ensure_fleet(&ws).await {
                Ok(v) => {
                    project_name.set(v["name"].as_str().unwrap_or("Workspace fleet").to_string());
                    if let Some(b) = v["sandbox_backend"].as_str() {
                        sandbox_backend.set(b.to_string());
                    }
                    status_line.set(format!(
                        "Fleet ready · sandbox {} · project {}",
                        v["sandbox_backend"].as_str().unwrap_or("stub"),
                        v["project_id"].as_str().unwrap_or(&ws)
                    ));
                    ready.set(true);
                    if let Ok(t) = list_tasks(&ws).await {
                        tasks.set(parse_tasks(&t));
                    }
                    if let Ok(r) = list_runs(&ws).await {
                        runs.set(parse_runs(&r));
                    }
                    if let Ok(s) = list_sessions(&ws).await {
                        sessions.set(parse_sessions(&s));
                    }
                }
                Err(e) => {
                    status_line.set(e.message());
                    ready.set(false);
                }
            }
        });
    });

    let ws_poll = workspace_id.clone();
    use_future(move || {
        let ws = ws_poll.clone();
        async move {
            loop {
                TimeoutFuture::new(2500).await;
                if let Ok(t) = list_tasks(&ws).await {
                    tasks.set(parse_tasks(&t));
                }
                if let Ok(r) = list_runs(&ws).await {
                    runs.set(parse_runs(&r));
                }
                if let Ok(s) = list_sessions(&ws).await {
                    sessions.set(parse_sessions(&s));
                }
            }
        }
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
                    p { class: "mt-0.5 font-mono text-xs text-[var(--app-muted)]",
                        "workspace {workspace_id} · jobs run on paired hosts · plane is not a runner"
                    }
                    p { class: "mt-0.5 text-xs text-[var(--app-muted)]",
                        "SANDBOX_BACKEND={sandbox_backend} is display-only."
                    }
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
                                    if let Ok(s) = list_sessions(&ws).await {
                                        sessions.set(parse_sessions(&s));
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
                                    if let Ok(s) = list_sessions(&ws).await {
                                        sessions.set(parse_sessions(&s));
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                        "Run tick"
                    }
                    button {
                        class: "rounded-md bg-indigo-600 px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50",
                        disabled: busy() || !ready(),
                        r#type: "button",
                        onclick: {
                            let workspace_id = workspace_id.clone();
                            move |_| {
                                busy.set(true);
                                let ws = workspace_id.clone();
                                spawn(async move {
                                    let mut n = 0u32;
                                    let max = 24u32;
                                    loop {
                                        n += 1;
                                        match run_tick(&ws).await {
                                            Ok(v) => {
                                                last_tick.set(
                                                    serde_json::to_string_pretty(&v)
                                                        .unwrap_or_default(),
                                                );
                                            }
                                            Err(e) => {
                                                status_line.set(e.message());
                                                break;
                                            }
                                        }
                                        let runs_v = list_runs(&ws).await.ok();
                                        let tasks_v = list_tasks(&ws).await.ok();
                                        let sess_v = list_sessions(&ws).await.ok();
                                        if let Some(ref r) = runs_v {
                                            runs.set(parse_runs(r));
                                        }
                                        if let Some(ref t) = tasks_v {
                                            tasks.set(parse_tasks(t));
                                        }
                                        if let Some(ref s) = sess_v {
                                            sessions.set(parse_sessions(s));
                                        }
                                        let terminal = runs_v
                                            .as_ref()
                                            .and_then(|r| r["runs"].as_array())
                                            .map(|arr| {
                                                !arr.is_empty()
                                                    && arr.iter().all(|x| {
                                                        run_is_terminal(
                                                            x["status"].as_str().unwrap_or(""),
                                                        )
                                                    })
                                            })
                                            .unwrap_or(false);
                                        if terminal {
                                            status_line.set(format!(
                                                "Run finished after {n} tick(s)"
                                            ));
                                            break;
                                        }
                                        if n >= max {
                                            status_line.set(format!(
                                                "Stopped after {max} ticks — press Run tick or refresh"
                                            ));
                                            break;
                                        }
                                        status_line.set(format!("Orchestrating… tick {n}/{max}"));
                                        TimeoutFuture::new(400).await;
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                        "Run until done"
                    }
                    button {
                        class: "rounded-md border border-[var(--app-border)] px-3 py-1.5 text-sm font-medium text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)] disabled:opacity-50",
                        disabled: busy() || !ready(),
                        r#type: "button",
                        onclick: {
                            move |_| {
                                refresh();
                                status_line.set("Board refreshed.".into());
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
                                    if let Ok(s) = list_sessions(&ws).await {
                                        sessions.set(parse_sessions(&s));
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

            section { class: "rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 mb-2 text-sm font-semibold uppercase tracking-wide text-[var(--app-muted)]", "Sandboxes / sessions" }
                if sessions().is_empty() {
                    p { class: "text-sm text-[var(--app-muted)]",
                        "No worker sessions yet. Pair a host under Runners, then tick. "
                        "The control plane does not spawn a local stub guest."
                    }
                } else {
                    div { class: "flex flex-col gap-2",
                        for s in sessions() {
                            div {
                                key: "{s.id}",
                                class: "flex flex-wrap items-center justify-between gap-2 rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2",
                                div { class: "min-w-0",
                                    p { class: "m-0 font-mono text-xs text-[var(--app-muted)]", "{s.id}" }
                                    p { class: "m-0 text-sm text-[var(--app-fg)]",
                                        "{s.runtime_backend} · {s.worker_profile}"
                                    }
                                    if !s.work_dir.is_empty() {
                                        p { class: "m-0 truncate font-mono text-[10px] text-[var(--app-muted)]", "{s.work_dir}" }
                                    }
                                    if !s.exit_reason.is_empty() {
                                        p { class: "m-0 text-xs text-red-400", "{s.exit_reason}" }
                                    }
                                }
                                div { class: "flex items-center gap-2",
                                    Link {
                                        to: Route::SessionDetail {
                                            workspace_id: workspace_id.clone(),
                                            session_id: s.id.clone(),
                                        },
                                        class: "text-[11px] text-[var(--app-accent)] hover:underline",
                                        "Open"
                                    }
                                    span { class: "text-[10px] text-[var(--app-muted)]", "{s.progress_pct}%" }
                                    span {
                                        class: "rounded-full border px-2 py-0.5 text-xs font-medium {status_pill_class(&s.status)}",
                                        "{s.status}"
                                    }

                                }
                            }
                        }
                    }
                }
            }

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
                                                    span { class: "text-[10px] text-[var(--app-muted)]", "{t.progress_pct}%" }
                                                    span { class: "text-[10px] text-[var(--app-muted)]", "{t.worker_profile}" }
                                                }
                                                if t.status == "blocked" {
                                                    if !t.block_reason.is_empty() {
                                                        p { class: "m-0 mt-1 text-[11px] text-amber-300/90", "{t.block_reason}" }
                                                    }
                                                    if !t.hitl_notes.is_empty() {
                                                        p { class: "m-0 mt-0.5 text-[11px] text-[var(--app-muted)]", "{t.hitl_notes}" }
                                                    }
                                                    input {
                                                        class: "mt-2 w-full rounded-md border border-[var(--app-border)] bg-[var(--app-bg)] px-2 py-1 text-[11px] text-[var(--app-fg)]",
                                                        r#type: "text",
                                                        placeholder: "Resume note (optional)",
                                                        value: "{resume_note}",
                                                        oninput: move |e| resume_note.set(e.value()),
                                                    }
                                                    button {
                                                        class: "mt-1 rounded-md bg-amber-600 px-2 py-1 text-[11px] font-medium text-white disabled:opacity-50",
                                                        r#type: "button",
                                                        disabled: busy(),
                                                        onclick: {
                                                            let workspace_id = workspace_id.clone();
                                                            let tid = t.id.clone();
                                                            move |_| {
                                                                if busy() { return; }
                                                                busy.set(true);
                                                                let ws = workspace_id.clone();
                                                                let tid = tid.clone();
                                                                let note = resume_note();
                                                                spawn(async move {
                                                                    match resume_task(&ws, &tid, &note).await {
                                                                        Ok(_) => {
                                                                            resume_note.set(String::new());
                                                                            status_line.set("HITL resume requested".into());
                                                                        }
                                                                        Err(e) => status_line.set(e.message()),
                                                                    }
                                                                    if let Ok(t) = list_tasks(&ws).await {
                                                                        tasks.set(parse_tasks(&t));
                                                                    }
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
                strong { "Run until done" }
                " (or tick manually). Guests are "
                span { class: "font-mono", "fleet-driver" }
                " on a paired "
                span { class: "font-mono", "fleet-host" }
                " (process, container, or kube pod)."
            }
        }
    }
}

#[cfg(test)]
mod tests {
    // catalog:list-fleet-ui-fixture-is-chain-not-diamond
    // Docs §6.5: Start 2-task run calls start_fixture (kind=chain).
    // UI never sends diamond. Does not tick.
    // Host: cargo test -p fleet-ui --locked -- chain

    fn production_page() -> &'static str {
        include_str!("page.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("fleet page.rs production source")
    }

    fn production_api() -> &'static str {
        include_str!("api.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("fleet api.rs production source")
    }

    fn button_named<'a>(src: &'a str, label: &str) -> &'a str {
        let at = src.find(label).unwrap_or_else(|| panic!("missing {label}"));
        let before = &src[..at];
        let start = before
            .rfind("button {")
            .unwrap_or_else(|| panic!("{label} must sit on a button {{ ... }}"));
        let after = &src[at + label.len()..];
        let end = after.find("button {").unwrap_or(after.len());
        &src[start..at + label.len() + end]
    }

    #[test]
    fn start_2_task_button_posts_chain_fixture_and_does_not_tick() {
        let page = production_page();
        let api = production_api();
        assert!(
            page.contains("\"Start 2-task run\""),
            "Fleet must expose Start 2-task run"
        );
        let start_btn = button_named(page, "\"Start 2-task run\"");
        assert!(
            start_btn.contains("start_fixture"),
            "Start 2-task run must call start_fixture:\n{start_btn}"
        );
        assert!(
            !start_btn.contains("run_tick") && !start_btn.contains("/tick"),
            "docs §6.5: Start 2-task must not tick:\n{start_btn}"
        );
        assert!(
            !start_btn.to_ascii_lowercase().contains("diamond"),
            "Start 2-task must never send diamond:\n{start_btn}"
        );
        assert!(
            !page.to_ascii_lowercase().contains("diamond"),
            "Fleet page must never send or offer diamond"
        );

        let tick_btn = button_named(page, "\"Run tick\"");
        assert!(
            tick_btn.contains("run_tick"),
            "Run tick is the separate tick control:\n{tick_btn}"
        );
        assert!(
            !tick_btn.contains("start_fixture") && !tick_btn.contains("/runs/fixture"),
            "Run tick must not start a fixture:\n{tick_btn}"
        );

        assert!(
            api.contains("pub async fn start_fixture"),
            "page start_fixture must be the fleet API helper"
        );
        assert!(
            api.contains("fn start_fixture_body"),
            "Start 2-task must post start_fixture_body (kind=chain)"
        );
    }
}
