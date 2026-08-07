use dioxus::prelude::*;
use serde_json::Value;

use super::api::list_sessions;

#[derive(Clone, PartialEq)]
struct SessionRow {
    id: String,
    status: String,
    backend: String,
    profile: String,
    progress: i64,
    work_dir: String,
    exit_reason: String,
}

fn parse(v: &Value) -> (String, Vec<SessionRow>) {
    let backend = v["sandbox_backend"]
        .as_str()
        .unwrap_or("unknown")
        .to_string();
    let rows = v["sessions"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    Some(SessionRow {
                        id: s["id"].as_str()?.to_string(),
                        status: s["status"].as_str().unwrap_or("?").to_string(),
                        backend: s["runtime_backend"].as_str().unwrap_or("?").to_string(),
                        profile: s["worker_profile"].as_str().unwrap_or("").to_string(),
                        progress: s["progress_pct"].as_i64().unwrap_or(0),
                        work_dir: s["work_dir"].as_str().unwrap_or("").to_string(),
                        exit_reason: s["exit_reason"].as_str().unwrap_or("").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    (backend, rows)
}

#[component]
pub fn Sandboxes(workspace_id: String) -> Element {
    let mut backend = use_signal(|| String::new());
    let mut rows = use_signal(|| Vec::<SessionRow>::new());
    let mut error = use_signal(|| Option::<String>::None);
    let mut loading = use_signal(|| true);

    {
        let ws = workspace_id.clone();
        use_effect(move || {
            let ws = ws.clone();
            spawn(async move {
                loading.set(true);
                match list_sessions(&ws).await {
                    Ok(v) => {
                        let (b, list) = parse(&v);
                        backend.set(b);
                        rows.set(list);
                        error.set(None);
                    }
                    Err(e) => error.set(Some(e.message())),
                }
                loading.set(false);
            });
        });
    }

    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-6xl flex-col gap-4 overflow-auto p-4 md:p-5",
            div { class: "flex flex-wrap items-start justify-between gap-3",
                div {
                    h1 { class: "m-0 text-2xl font-bold tracking-tight text-[var(--app-fg)]", "Sandboxes" }
                    p { class: "mt-1 text-sm text-[var(--app-muted)]",
                        "Worker sessions spawned by the orchestrator for this workspace."
                    }
                    p { class: "mt-0.5 font-mono text-xs text-[var(--app-muted)]",
                        "runtime "
                        if backend().is_empty() { "—" } else { "{backend}" }
                    }
                }
                button {
                    class: "rounded-md border border-[var(--app-border)] px-3 py-1.5 text-sm font-medium text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)]",
                    r#type: "button",
                    onclick: {
                        let workspace_id = workspace_id.clone();
                        move |_| {
                            let ws = workspace_id.clone();
                            spawn(async move {
                                loading.set(true);
                                match list_sessions(&ws).await {
                                    Ok(v) => {
                                        let (b, list) = parse(&v);
                                        backend.set(b);
                                        rows.set(list);
                                        error.set(None);
                                    }
                                    Err(e) => error.set(Some(e.message())),
                                }
                                loading.set(false);
                            });
                        }
                    },
                    "Refresh"
                }
            }

            if let Some(msg) = error() {
                p { class: "text-sm text-red-400", "{msg}" }
            } else if loading() && rows().is_empty() {
                p { class: "text-sm text-[var(--app-muted)]", "Loading sessions…" }
            } else if rows().is_empty() {
                p { class: "text-sm text-[var(--app-muted)]",
                    "No sandboxes yet. Start a run on the Fleet page and tick until a worker spawns."
                }
            } else {
                div { class: "flex flex-col gap-2",
                    for s in rows() {
                        div {
                            key: "{s.id}",
                            class: "rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                            div { class: "flex flex-wrap items-center justify-between gap-2",
                                p { class: "m-0 font-medium text-[var(--app-fg)]", "{s.backend} · {s.profile}" }
                                span { class: "text-xs text-[var(--app-muted)]", "{s.status} · {s.progress}%" }
                            }
                            p { class: "m-0 mt-1 font-mono text-xs text-[var(--app-muted)]", "{s.id}" }
                            if !s.work_dir.is_empty() {
                                p { class: "m-0 mt-1 break-all font-mono text-[11px] text-[var(--app-muted)]", "{s.work_dir}" }
                            }
                            if !s.exit_reason.is_empty() {
                                p { class: "m-0 mt-1 text-sm text-red-400", "{s.exit_reason}" }
                            }
                        }
                    }
                }
            }
        }
    }
}
