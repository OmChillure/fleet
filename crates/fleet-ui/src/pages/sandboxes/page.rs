//! Live sandbox list (worker sessions via cookie fleet API).

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

use super::api::{SandboxRow, list_sandboxes};
use super::shape::{
    EMPTY_HEADING, EMPTY_HINT, LOAD_ERROR, PAGE_HEADING, PAGE_SUBTITLE, list_status_line,
    sandbox_card_model,
};
use crate::Route;

#[component]
pub fn Sandboxes(workspace_id: String) -> Element {
    let mut rows = use_signal(|| Vec::<SandboxRow>::new());
    let mut backend = use_signal(String::new);
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
                    backend.set(resp.display_backend());
                    let n = resp.sandboxes.len();
                    rows.set(resp.sandboxes);
                    status_line.set(list_status_line(n));
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
                    backend.set(resp.display_backend());
                    rows.set(resp.sandboxes);
                }
            }
        }
    });

    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-6xl flex-col gap-4 overflow-auto p-4 md:p-5",
            div { class: "flex flex-wrap items-start justify-between gap-3",
                div {
                    h1 { class: "m-0 text-2xl font-bold tracking-tight text-[var(--app-fg)]",
                        "{PAGE_HEADING}"
                    }
                    p { class: "mt-1 text-sm text-[var(--app-muted)]",
                        "{PAGE_SUBTITLE}"
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
                                            backend.set(resp.display_backend());
                                            let n = resp.sandboxes.len();
                                            rows.set(resp.sandboxes);
                                            status_line.set(list_status_line(n));
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
                    "{LOAD_ERROR}"
                }
            } else if rows().is_empty() {
                div {
                    class: "rounded-xl border border-dashed border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-8 text-center",
                    p { class: "m-0 text-sm font-medium text-[var(--app-fg)]", "{EMPTY_HEADING}" }
                    p { class: "mt-2 text-sm text-[var(--app-muted)]",
                        "{EMPTY_HINT}"
                    }
                }
            } else {
                div { class: "flex flex-col gap-2",
                    for s in rows() {
                        SandboxCard {
                            key: "{s.id}",
                            workspace_id: workspace_id.clone(),
                            row: s,
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn SandboxCard(workspace_id: String, row: SandboxRow) -> Element {
    let card = sandbox_card_model(&row);
    let pill = card.pill_class;
    rsx! {
        div {
            class: "rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
            div { class: "flex flex-wrap items-start justify-between gap-2",
                div { class: "flex flex-wrap items-center gap-2",
                    span {
                        class: "inline-flex items-center rounded-full border px-2 py-0.5 text-xs font-medium {pill}",
                        "{card.status}"
                    }
                    span { class: "text-sm font-medium text-[var(--app-fg)]",
                        "{card.worker_profile}"
                    }
                    span { class: "font-mono text-xs text-[var(--app-muted)]",
                        "{card.runtime_backend}"
                    }
                }
                div { class: "flex items-center gap-2",
                    span { class: "text-xs text-[var(--app-muted)]",
                        "{card.progress_label}"
                    }
                    Link {
                        to: Route::SessionDetail {
                            workspace_id: workspace_id.clone(),
                            session_id: row.id.clone(),
                        },
                        class: "text-[11px] text-[var(--app-accent)] underline",
                        "Watch"
                    }
                }
            }
            div { class: "mt-3 grid gap-1 whitespace-pre font-mono text-xs text-[var(--app-muted)] sm:grid-cols-2",
                div { "session  {card.session_short}…" }
                div { "run      {card.run_short}…" }
                div { "task     {card.task_short}…" }
                div { "attempt  {card.attempt}" }
                div { "started  {card.started}" }
                div { "exited   {card.exited}" }
            }
            if let Some(reason) = &card.exit_reason {
                p { class: "mt-2 text-xs text-amber-300/90", "exit: {reason}" }
            }
            if let Some(dir) = &card.work_dir {
                p { class: "mt-1 truncate font-mono text-[10px] text-[var(--app-muted)]",
                    "{dir}"
                }
            }
        }
    }
}
