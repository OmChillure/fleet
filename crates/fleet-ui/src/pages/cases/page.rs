//! Cases list + detail (stage, events, observe / cancel).

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

use super::api::{
    cancel_case, get_case, list_cases, observe_case, CaseEventRow, CaseRow,
};
use crate::Route;

fn short(id: &str) -> String {
    id.get(..8).unwrap_or(id).to_string()
}

fn status_class(status: &str) -> &'static str {
    match status {
        "completed" => "bg-emerald-500/15 text-emerald-400 border-emerald-500/30",
        "failed" | "cancelled" => "bg-red-500/15 text-red-400 border-red-500/30",
        "acting" | "deciding" => "bg-sky-500/15 text-sky-400 border-sky-500/30",
        "waiting" | "open" => "bg-amber-500/15 text-amber-400 border-amber-500/30",
        _ => "bg-[var(--app-bg-hover)] text-[var(--app-muted)] border-[var(--app-border)]",
    }
}

fn format_ts(ts: &Option<String>) -> String {
    match ts {
        Some(s) if !s.is_empty() => s.get(..19).unwrap_or(s).replace('T', " "),
        _ => "—".into(),
    }
}

#[component]
pub fn Cases(workspace_id: String) -> Element {
    cases_view(workspace_id, None)
}

#[component]
pub fn CaseDetail(workspace_id: String, case_id: String) -> Element {
    cases_view(workspace_id, Some(case_id))
}

fn cases_view(workspace_id: String, selected: Option<String>) -> Element {
    let mut rows = use_signal(Vec::<CaseRow>::new);
    let mut detail = use_signal(|| None::<CaseRow>);
    let mut status_line = use_signal(|| "Loading cases…".to_string());
    let note = use_signal(String::new);
    let busy = use_signal(|| false);
    let selected_id = selected.clone();

    let ws_list = workspace_id.clone();
    use_future(move || {
        let ws = ws_list.clone();
        async move {
            loop {
                match list_cases(&ws).await {
                    Ok(resp) => {
                        let n = resp.cases.len();
                        rows.set(resp.cases);
                        status_line.set(format!("{n} case(s)"));
                    }
                    Err(e) => status_line.set(e.message()),
                }
                TimeoutFuture::new(3000).await;
            }
        }
    });

    let ws_detail = workspace_id.clone();
    let sel_detail = selected_id.clone();
    use_future(move || {
        let ws = ws_detail.clone();
        let id = sel_detail.clone();
        async move {
            let Some(cid) = id else {
                detail.set(None);
                return;
            };
            loop {
                if let Ok(c) = get_case(&ws, &cid).await {
                    detail.set(Some(c));
                }
                TimeoutFuture::new(2500).await;
            }
        }
    });

    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-6xl flex-col gap-4 overflow-auto p-4 md:p-5",
            div {
                h1 { class: "m-0 text-2xl font-bold tracking-tight text-[var(--app-fg)]",
                    "Cases"
                }
                p { class: "mt-1 text-sm text-[var(--app-muted)]",
                    "Durable workflows. The backend wakes on a schedule; the LLM decides the next action (Slack / GitHub / Notion tools, or a fleet run)."
                }
                p { class: "mt-0.5 font-mono text-xs text-[var(--app-muted)]",
                    "{status_line}"
                }
            }

            div { class: "grid gap-4 lg:grid-cols-[minmax(0,18rem)_1fr]",
                section { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-3",
                    h2 { class: "m-0 px-1 text-sm font-semibold text-[var(--app-fg)]", "Open & recent" }
                    if rows().is_empty() {
                        p { class: "mt-3 px-1 text-sm text-[var(--app-muted)]",
                            "No cases yet. In Chat: start_case with a policy, or ask the assistant to watch Slack / book a follow-up."
                        }
                    } else {
                        ul { class: "mt-2 m-0 list-none space-y-1 p-0",
                            for c in rows() {
                                li { key: "{c.id}",
                                    Link {
                                        to: Route::CaseDetail {
                                            workspace_id: workspace_id.clone(),
                                            case_id: c.id.clone(),
                                        },
                                        class: if selected_id.as_deref() == Some(c.id.as_str()) {
                                            "block rounded-xl border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2"
                                        } else {
                                            "block rounded-xl border border-transparent px-3 py-2 hover:bg-[var(--app-bg-hover)]"
                                        },
                                        p { class: "m-0 text-sm text-[var(--app-fg)]", "{c.title}" }
                                        p { class: "m-0 mt-0.5 font-mono text-[11px] text-[var(--app-muted)]",
                                            "{short(&c.id)} · {c.stage}"
                                        }
                                        span {
                                            class: "mt-1 inline-block rounded-md border px-1.5 py-0.5 text-[10px] uppercase tracking-wide {status_class(&c.status)}",
                                            "{c.status}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                section { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                    if let Some(c) = detail() {
                        CasePanel {
                            workspace_id: workspace_id.clone(),
                            case: c,
                            note: note,
                            busy: busy,
                        }
                    } else if selected_id.is_some() {
                        p { class: "m-0 text-sm text-[var(--app-muted)]", "Loading case…" }
                    } else {
                        p { class: "m-0 text-sm text-[var(--app-muted)]",
                            "Select a case to see stage, events, and inject a fact."
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn CasePanel(
    workspace_id: String,
    case: CaseRow,
    mut note: Signal<String>,
    mut busy: Signal<bool>,
) -> Element {
    let cid = case.id.clone();
    let events: Vec<CaseEventRow> = case.events.clone();
    let terminal = matches!(case.status.as_str(), "completed" | "failed" | "cancelled");

    rsx! {
        div { class: "flex flex-col gap-3",
            div { class: "flex flex-wrap items-start justify-between gap-2",
                div {
                    h2 { class: "m-0 text-lg font-semibold text-[var(--app-fg)]", "{case.title}" }
                    p { class: "m-0 mt-1 font-mono text-[11px] text-[var(--app-muted)]",
                        "{case.id} · kind {case.kind} · decisions {case.decision_count}"
                    }
                }
                span {
                    class: "rounded-md border px-2 py-0.5 text-xs uppercase {status_class(&case.status)}",
                    "{case.status} · {case.stage}"
                }
            }

            if let Some(err) = case.last_error.clone() {
                p { class: "m-0 text-sm text-red-400", "{err}" }
            }
            if let Some(run) = case.run_id.clone() {
                p { class: "m-0 font-mono text-xs text-[var(--app-muted)]", "fleet run {run}" }
            }
            if !case.policy_md.is_empty() {
                pre { class: "m-0 max-h-40 overflow-auto whitespace-pre-wrap rounded-xl bg-[var(--app-bg)] p-3 text-xs text-[var(--app-fg)]",
                    "{case.policy_md}"
                }
            }

            if !terminal {
                div { class: "flex flex-col gap-2 sm:flex-row",
                    input {
                        class: "min-w-0 flex-1 rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                        r#type: "text",
                        placeholder: "Observe a fact (wake the case)",
                        value: "{note}",
                        oninput: move |e| note.set(e.value()),
                    }
                    button {
                        class: "rounded-lg bg-[var(--app-fg)] px-3 py-2 text-sm font-semibold text-[var(--app-bg)] disabled:opacity-50",
                        r#type: "button",
                        disabled: busy() || note().trim().is_empty(),
                        onclick: {
                            let ws = workspace_id.clone();
                            let id = cid.clone();
                            move |_| {
                                if busy() { return; }
                                let n = note().trim().to_string();
                                if n.is_empty() { return; }
                                busy.set(true);
                                let ws = ws.clone();
                                let id = id.clone();
                                spawn(async move {
                                    let _ = observe_case(&ws, &id, &n).await;
                                    busy.set(false);
                                    note.set(String::new());
                                });
                            }
                        },
                        "Observe"
                    }
                    button {
                        class: "rounded-lg border border-[var(--app-border)] px-3 py-2 text-sm text-[var(--app-muted)] hover:text-[var(--app-fg)] disabled:opacity-50",
                        r#type: "button",
                        disabled: busy(),
                        onclick: {
                            let ws = workspace_id.clone();
                            let id = cid.clone();
                            move |_| {
                                if busy() { return; }
                                busy.set(true);
                                let ws = ws.clone();
                                let id = id.clone();
                                spawn(async move {
                                    let _ = cancel_case(&ws, &id).await;
                                    busy.set(false);
                                });
                            }
                        },
                        "Cancel"
                    }
                }
            }

            div {
                h3 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Events" }
                if events.is_empty() {
                    p { class: "mt-2 text-sm text-[var(--app-muted)]", "No events yet." }
                } else {
                    ul { class: "mt-2 m-0 list-none space-y-2 p-0",
                        for ev in events {
                            li { key: "{ev.id}",
                                class: "rounded-xl bg-[var(--app-bg)] px-3 py-2",
                                p { class: "m-0 text-xs font-medium text-[var(--app-fg)]", "{ev.kind}" }
                                p { class: "m-0 font-mono text-[11px] text-[var(--app-muted)]",
                                    "{format_ts(&ev.created_at)}"
                                }
                                pre { class: "m-0 mt-1 max-h-28 overflow-auto whitespace-pre-wrap text-[11px] text-[var(--app-muted)]",
                                    "{ev.payload}"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
