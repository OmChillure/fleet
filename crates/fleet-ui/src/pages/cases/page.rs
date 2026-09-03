//! Cases list + detail (stage, events, observe / cancel).

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;

use super::api::{
    CaseEventRow, CaseRow, cancel_case, create_case, decide_case, get_case, list_cases,
    observe_case, wake_case,
};
use crate::Route;
use crate::components::toast::show_toast;

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
    let mut new_title = use_signal(String::new);
    let mut new_policy = use_signal(String::new);
    let mut new_kind = use_signal(|| "generic".to_string());
    let mut creating = use_signal(|| false);
    let selected_id = selected.clone();
    let navigator = use_navigator();
    let mut selected_sig = use_signal(|| selected.clone());
    if selected_sig() != selected {
        selected_sig.set(selected.clone());
        let keep = match (detail(), selected.as_ref()) {
            (Some(c), Some(id)) if &c.id == id => true,
            _ => false,
        };
        if !keep {
            if let Some(id) = selected.as_ref() {
                if let Some(row) = rows().into_iter().find(|c| &c.id == id) {
                    detail.set(Some(row));
                } else {
                    detail.set(None);
                }
            } else {
                detail.set(None);
            }
        }
    }

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

    let ws_effect = workspace_id.clone();
    use_effect(move || {
        let ws = ws_effect.clone();
        let id = selected_sig();
        spawn(async move {
            let Some(cid) = id else {
                return;
            };
            if let Ok(c) = get_case(&ws, &cid).await {
                detail.set(Some(c));
            }
        });
    });
    let ws_poll = workspace_id.clone();
    use_future(move || {
        let ws = ws_poll.clone();
        let id = selected_sig();
        async move {
            let Some(cid) = id else {
                return;
            };
            loop {
                TimeoutFuture::new(2500).await;
                if let Ok(c) = get_case(&ws, &cid).await {
                    detail.set(Some(c));
                }
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

            section { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "New case" }
                p { class: "mt-1 text-xs text-[var(--app-muted)]",
                    "Title + a policy in plain language. The wake loop owns status; you can observe facts later."
                }
                input {
                    class: "mt-3 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                    r#type: "text",
                    placeholder: "Title — e.g. Watch Slack #support for refunds",
                    value: "{new_title}",
                    oninput: move |e| new_title.set(e.value()),
                }
                select {
                    class: "mt-2 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                    value: "{new_kind}",
                    onchange: move |e| new_kind.set(e.value()),
                    option { value: "generic", "generic" }
                    option { value: "ticket", "ticket" }
                    option { value: "outreach", "outreach" }
                    option { value: "watch", "watch" }
                }
                textarea {
                    class: "mt-2 min-h-24 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                    placeholder: "Policy — when to wait, what counts as done, which tools to use…",
                    value: "{new_policy}",
                    oninput: move |e| new_policy.set(e.value()),
                }
                button {
                    class: "mt-3 rounded-lg bg-[var(--app-fg)] px-3 py-2 text-sm font-semibold text-[var(--app-bg)] disabled:opacity-50",
                    r#type: "button",
                    disabled: creating() || new_title().trim().is_empty() || new_policy().trim().is_empty(),
                    onclick: {
                        let workspace_id = workspace_id.clone();
                        move |_| {
                            if creating() { return; }
                            let title = new_title().trim().to_string();
                            let policy = new_policy().trim().to_string();
                            if title.is_empty() || policy.is_empty() { return; }
                            creating.set(true);
                            let ws = workspace_id.clone();
                            let kind = new_kind();
                            spawn(async move {
                                match create_case(&ws, &title, &policy, Some(kind.as_str())).await {
                                    Ok(c) => {
                                        new_title.set(String::new());
                                        new_policy.set(String::new());
                                        show_toast().success("Case created.");
                                        let cid = c.id.clone();
                                        detail.set(Some(c));
                                        navigator.push(Route::CaseDetail {
                                            workspace_id: ws,
                                            case_id: cid,
                                        });
                                    }
                                    Err(e) => {
                                        show_toast().error(e.message());
                                        status_line.set(e.message());
                                    }
                                }
                                creating.set(false);
                            });
                        }
                    },
                    if creating() { "Creating…" } else { "Create case" }
                }
            }

            div { class: "grid gap-4 lg:grid-cols-[minmax(0,18rem)_1fr]",
                section { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-3",
                    h2 { class: "m-0 px-1 text-sm font-semibold text-[var(--app-fg)]", "Open & recent" }
                    if rows().is_empty() {
                        p { class: "mt-3 px-1 text-sm text-[var(--app-muted)]",
                            "No cases yet. Use the form above, or ask Chat to start one."
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
                            detail: detail,
                            rows: rows,
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

async fn reload_case(
    ws: &str,
    id: &str,
    mut detail: Signal<Option<CaseRow>>,
    mut rows: Signal<Vec<CaseRow>>,
) {
    if let Ok(c) = get_case(ws, id).await {
        let mut list = rows();
        if let Some(slot) = list.iter_mut().find(|r| r.id == c.id) {
            slot.status = c.status.clone();
            slot.stage = c.stage.clone();
            slot.title = c.title.clone();
        }
        rows.set(list);
        detail.set(Some(c));
    }
}

#[component]
fn CasePanel(
    workspace_id: String,
    case: CaseRow,
    mut note: Signal<String>,
    mut busy: Signal<bool>,
    detail: Signal<Option<CaseRow>>,
    rows: Signal<Vec<CaseRow>>,
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
                div { class: "flex flex-wrap items-center gap-1.5",
                    span {
                        class: "rounded-md border px-2 py-0.5 text-xs uppercase {status_class(&case.status)}",
                        "{case.status}"
                    }
                    span {
                        class: "rounded-md border border-[var(--app-border)] px-2 py-0.5 text-xs text-[var(--app-muted)]",
                        "{case.stage}"
                    }
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
                                    match observe_case(&ws, &id, &n).await {
                                        Ok(_) => {
                                            note.set(String::new());
                                            show_toast().success("Observation recorded.");
                                            reload_case(&ws, &id, detail, rows).await;
                                        }
                                        Err(e) => show_toast().error(e.message()),
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                        "Observe"
                    }
                    button {
                        class: "rounded-lg border border-[var(--app-border)] px-3 py-2 text-sm font-medium text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)] disabled:opacity-50",
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
                                    match wake_case(&ws, &id).await {
                                        Ok(_) => {
                                            show_toast().success("Decision applied.");
                                            reload_case(&ws, &id, detail, rows).await;
                                        }
                                        Err(e) => show_toast().error(e.message()),
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                        "Decide now"
                    }
                    button {
                        class: "rounded-lg border border-emerald-500/30 px-3 py-2 text-sm text-emerald-300 hover:bg-emerald-500/10 disabled:opacity-50",
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
                                    match decide_case(&ws, &id, "complete", Some("completed from UI"), None).await {
                                        Ok(_) => {
                                            show_toast().success("Case completed.");
                                            reload_case(&ws, &id, detail, rows).await;
                                        }
                                        Err(e) => show_toast().error(e.message()),
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                        "Complete"
                    }
                    button {
                        class: "rounded-lg border border-red-500/30 px-3 py-2 text-sm text-red-400 hover:bg-red-500/10 disabled:opacity-50",
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
                                    match decide_case(&ws, &id, "fail", Some("failed from UI"), None).await {
                                        Ok(_) => {
                                            show_toast().success("Case failed.");
                                            reload_case(&ws, &id, detail, rows).await;
                                        }
                                        Err(e) => show_toast().error(e.message()),
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                        "Fail"
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
                                    match cancel_case(&ws, &id).await {
                                        Ok(_) => {
                                            show_toast().success("Case cancelled.");
                                            reload_case(&ws, &id, detail, rows).await;
                                        }
                                        Err(e) => show_toast().error(e.message()),
                                    }
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

#[cfg(test)]
mod tests {
    #[test]
    fn detail_status_and_stage_are_separate_pills() {
        let src = include_str!("page.rs");
        let prod = src.split("#[cfg(test)]").next().expect("prod");
        assert!(
            !prod.contains("\"{case.status} · {case.stage}\""),
            "detail must not jam status and stage into one uppercase pill"
        );
        assert!(
            prod.contains("\"{case.status}\""),
            "detail must render status on its own"
        );
        assert!(
            prod.contains("\"{case.stage}\""),
            "detail must render stage on its own"
        );
    }
}
