//! Live sandbox list (worker sessions via cookie fleet API).

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

use super::api::{kill_sandbox, list_sandboxes, SandboxRow};

fn short_id(id: &str) -> String {
    id.get(..8).unwrap_or(id).to_string()
}

fn status_pill_class(status: &str) -> &'static str {
    match status {
        "healthy" | "spawning" => "bg-sky-500/15 text-sky-400 border-sky-500/30",
        "stuck" | "killing" => "bg-amber-500/15 text-amber-400 border-amber-500/30",
        "exited" => "bg-emerald-500/15 text-emerald-400 border-emerald-500/30",
        "failed_spawn" => "bg-red-500/15 text-red-400 border-red-500/30",
        _ => "bg-[var(--app-bg-hover)] text-[var(--app-muted)] border-[var(--app-border)]",
    }
}

fn format_ts(ts: &Option<String>) -> String {
    match ts {
        Some(s) if !s.is_empty() => {
            // Show date-time without full fractional precision if present
            s.get(..19).unwrap_or(s).replace('T', " ")
        }
        _ => "—".into(),
    }
}

#[component]
pub fn Sandboxes(workspace_id: String) -> Element {
    let mut rows = use_signal(|| Vec::<SandboxRow>::new());
    let mut backend = use_signal(|| String::new());
    let mut status_line = use_signal(|| "Loading sandboxes…".to_string());
    let mut busy = use_signal(|| false);
    let mut ready = use_signal(|| false);

    let ws_mount = workspace_id.clone();
    use_effect(move || {
        let ws = ws_mount.clone();
        busy.set(true);
        spawn(async move {
            match list_sandboxes(&ws).await {
                Ok(resp) => {
                    backend.set(if resp.sandbox_backend.is_empty() {
                        "stub".into()
                    } else {
                        resp.sandbox_backend
                    });
                    let n = resp.sandboxes.len();
                    rows.set(resp.sandboxes);
                    status_line.set(format!("{n} sandbox session(s)"));
                    ready.set(true);
                }
                Err(e) => {
                    status_line.set(e.message());
                    ready.set(false);
                }
            }
            busy.set(false);
        });
    });

    let ws_poll = workspace_id.clone();
    use_future(move || {
        let ws = ws_poll.clone();
        async move {
            loop {
                TimeoutFuture::new(2500).await;
                if let Ok(resp) = list_sandboxes(&ws).await {
                    rows.set(resp.sandboxes);
                    backend.set(if resp.sandbox_backend.is_empty() {
                        "stub".into()
                    } else {
                        resp.sandbox_backend
                    });
                }
            }
        }
    });

    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-6xl flex-col gap-4 overflow-auto p-4 md:p-5",
            div { class: "flex flex-wrap items-start justify-between gap-3",
                div {
                    h1 { class: "m-0 text-2xl font-bold tracking-tight text-[var(--app-fg)]",
                        "Sandboxes"
                    }
                    p { class: "mt-1 text-sm text-[var(--app-muted)]",
                        "Worker sessions spawned by fleet ticks (chat launch or Fleet fixture)."
                    }
                    p { class: "mt-0.5 font-mono text-xs text-[var(--app-muted)]",
                        "workspace {workspace_id}"
                        if !backend().is_empty() {
                            " · backend {backend}"
                        }
                    }
                }
                div { class: "flex flex-wrap gap-2",
                    button {
                        class: "rounded-md border border-[var(--app-border)] px-3 py-1.5 text-sm font-medium text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)] disabled:opacity-50",
                        disabled: busy(),
                        r#type: "button",
                        onclick: {
                            let workspace_id = workspace_id.clone();
                            move |_| {
                                busy.set(true);
                                let ws = workspace_id.clone();
                                spawn(async move {
                                    match list_sandboxes(&ws).await {
                                        Ok(resp) => {
                                            backend.set(if resp.sandbox_backend.is_empty() {
                                                "stub".into()
                                            } else {
                                                resp.sandbox_backend
                                            });
                                            let n = resp.sandboxes.len();
                                            rows.set(resp.sandboxes);
                                            status_line.set(format!("{n} sandbox session(s)"));
                                            ready.set(true);
                                        }
                                        Err(e) => {
                                            status_line.set(e.message());
                                            ready.set(false);
                                        }
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                        if busy() { "Refreshing…" } else { "Refresh" }
                    }
                }
            }

            p { class: "text-sm text-[var(--app-fg)]", "{status_line}" }

            if !ready() && rows().is_empty() {
                div {
                    class: "rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-8 text-center text-sm text-[var(--app-muted)]",
                    "Could not load sandboxes. Check API connectivity and workspace membership."
                }
            } else if rows().is_empty() {
                div {
                    class: "rounded-xl border border-dashed border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-8 text-center",
                    p { class: "m-0 text-sm font-medium text-[var(--app-fg)]", "No sandboxes yet" }
                    p { class: "mt-2 text-sm text-[var(--app-muted)]",
                        "From Chat, ask the assistant to launch work in a sandbox, or use Fleet → Start 2-task run → Run tick."
                    }
                }
            } else {
                div { class: "flex flex-col gap-2",
                    for s in rows() {
                        SandboxCard {
                            key: "{s.id}",
                            workspace_id: workspace_id.clone(),
                            row: s,
                            busy: busy,
                            status_line: status_line,
                            rows: rows,
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn SandboxCard(
    workspace_id: String,
    row: SandboxRow,
    mut busy: Signal<bool>,
    mut status_line: Signal<String>,
    mut rows: Signal<Vec<SandboxRow>>,
) -> Element {
    let pill = status_pill_class(&row.status);
    let live = matches!(
        row.status.as_str(),
        "spawning" | "healthy" | "stuck" | "killing"
    );
    rsx! {
        div {
            class: "rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
            div { class: "flex flex-wrap items-start justify-between gap-2",
                div { class: "flex flex-wrap items-center gap-2",
                    span {
                        class: "inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-medium {pill}",
                        "{row.status}"
                    }
                    span { class: "text-sm font-medium text-[var(--app-fg)]",
                        "{row.worker_profile}"
                    }
                    span { class: "font-mono text-xs text-[var(--app-muted)]",
                        "{row.runtime_backend}"
                    }
                }
                div { class: "flex items-center gap-2",
                    span { class: "text-xs text-[var(--app-muted)]",
                        "{row.progress_pct}%"
                    }
                    if live {
                        button {
                            class: "rounded-md border border-red-500/40 px-2 py-0.5 text-[11px] text-red-400 hover:bg-red-500/10 disabled:opacity-50",
                            r#type: "button",
                            disabled: busy(),
                            onclick: {
                                let workspace_id = workspace_id.clone();
                                let sid = row.id.clone();
                                move |_| {
                                    if busy() { return; }
                                    busy.set(true);
                                    let ws = workspace_id.clone();
                                    let sid = sid.clone();
                                    spawn(async move {
                                        match kill_sandbox(&ws, &sid).await {
                                            Ok(()) => {
                                                status_line.set(format!("Killed {sid}"));
                                                if let Ok(resp) = list_sandboxes(&ws).await {
                                                    rows.set(resp.sandboxes);
                                                }
                                            }
                                            Err(e) => status_line.set(e.message()),
                                        }
                                        busy.set(false);
                                    });
                                }
                            },
                            "Kill"
                        }
                    }
                }
            }
            div { class: "mt-3 grid gap-1 font-mono text-xs text-[var(--app-muted)] sm:grid-cols-2",
                div { "session  {short_id(&row.id)}…" }
                div { "run      {short_id(&row.run_id)}…" }
                div { "task     {short_id(&row.board_task_id)}…" }
                div { "attempt  {row.attempt}" }
                div { "started  {format_ts(&row.started_at)}" }
                div { "exited   {format_ts(&row.exited_at)}" }
            }
            if let Some(reason) = &row.exit_reason {
                if !reason.is_empty() {
                    p { class: "mt-2 text-xs text-amber-300/90", "exit: {reason}" }
                }
            }
            if let Some(dir) = &row.work_dir {
                if !dir.is_empty() {
                    p { class: "mt-1 truncate font-mono text-[10px] text-[var(--app-muted)]",
                        "{dir}"
                    }
                }
            }
        }
    }
}
