use dioxus::prelude::*;
use serde_json::Value;

use super::api::{get_session_detail, list_sessions};
use crate::Route;
use gloo_timers::future::TimeoutFuture;

#[derive(Clone, PartialEq)]
struct SessionRow {
    id: String,
    status: String,
    backend: String,
    profile: String,
    progress: i64,
    exit_reason: String,
}

fn parse_list(v: &Value) -> Vec<SessionRow> {
    v["sessions"]
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
                        exit_reason: s["exit_reason"].as_str().unwrap_or("").to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[component]
pub fn Sessions(workspace_id: String) -> Element {
    let mut rows = use_signal(|| Vec::<SessionRow>::new());
    let mut error = use_signal(|| Option::<String>::None);

    {
        let ws = workspace_id.clone();
        use_effect(move || {
            let ws = ws.clone();
            spawn(async move {
                match list_sessions(&ws).await {
                    Ok(v) => {
                        rows.set(parse_list(&v));
                        error.set(None);
                    }
                    Err(e) => error.set(Some(e.message())),
                }
            });
        });
    }

    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-5xl flex-col gap-4 overflow-auto p-4 md:p-5",
            div {
                h1 { class: "m-0 text-2xl font-bold tracking-tight text-[var(--app-fg)]", "Live sessions" }
                p { class: "mt-1 text-sm text-[var(--app-muted)]",
                    "Agent turns for this workspace. Open a session to watch the live stream."
                }
            }
            if let Some(err) = error() {
                p { class: "text-sm text-red-400", "{err}" }
            } else if rows().is_empty() {
                p { class: "text-sm text-[var(--app-muted)]",
                    "No sessions yet. Start a run on Fleet and tick until a worker spawns."
                }
            } else {
                div { class: "flex flex-col gap-2",
                    for s in rows() {
                        Link {
                            key: "{s.id}",
                            to: Route::SessionDetail {
                                workspace_id: workspace_id.clone(),
                                session_id: s.id.clone(),
                            },
                            class: "rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4 transition hover:border-[var(--app-accent)]",
                            div { class: "flex flex-wrap items-center justify-between gap-2",
                                p { class: "m-0 font-medium text-[var(--app-fg)]",
                                    "{s.backend} · {s.profile}"
                                }
                                span { class: "text-xs text-[var(--app-muted)]", "{s.status} · {s.progress}%" }
                            }
                            p { class: "m-0 mt-1 font-mono text-xs text-[var(--app-muted)]", "{s.id}" }
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

#[component]
pub fn SessionDetail(workspace_id: String, session_id: String) -> Element {
    let mut status = use_signal(|| "loading".to_string());
    let mut progress = use_signal(|| 0i64);
    let mut live = use_signal(|| String::new());
    let mut prompt = use_signal(|| String::new());
    let mut error = use_signal(|| Option::<String>::None);
    let mut running = use_signal(|| true);

    {
        let ws = workspace_id.clone();
        let sid = session_id.clone();
        use_effect(move || {
            let ws = ws.clone();
            let sid = sid.clone();
            spawn(async move {
                loop {
                    match get_session_detail(&ws, &sid).await {
                        Ok(v) => {
                            status.set(
                                v["session"]["status"]
                                    .as_str()
                                    .unwrap_or("?")
                                    .to_string(),
                            );
                            progress.set(v["session"]["progress_pct"].as_i64().unwrap_or(0));
                            prompt.set(v["prompt"].as_str().unwrap_or("").to_string());
                            let mut buf = String::new();
                            if let Some(arr) = v["live"].as_array() {
                                for ev in arr {
                                    if let Some(t) = ev["text"].as_str() {
                                        buf.push_str(t);
                                    }
                                }
                            }
                            if buf.trim().is_empty() {
                                buf = v["transcript"].as_str().unwrap_or("").to_string();
                            }
                            live.set(buf);
                            error.set(None);
                            let st = v["session"]["status"].as_str().unwrap_or("");
                            if matches!(st, "exited" | "failed" | "cancelled") {
                                running.set(false);
                                break;
                            }
                        }
                        Err(e) => error.set(Some(e.message())),
                    }
                    TimeoutFuture::new(1000).await;
                    if !running() {
                        break;
                    }
                }
            });
        });
    }

    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-5xl flex-col gap-3 overflow-auto p-4 md:p-5",
            div { class: "flex flex-wrap items-start justify-between gap-2",
                div {
                    Link {
                        class: "text-xs text-[var(--app-accent)] hover:underline",
                        to: Route::Sessions { workspace_id: workspace_id.clone() },
                        "← All sessions"
                    }
                    h1 { class: "m-0 mt-1 text-2xl font-bold tracking-tight text-[var(--app-fg)]", "Live session" }
                    p { class: "mt-0.5 font-mono text-xs text-[var(--app-muted)]", "{session_id}" }
                    p { class: "mt-0.5 text-sm text-[var(--app-muted)]", "{status} · {progress}%" }
                }
            }
            if let Some(err) = error() {
                p { class: "text-sm text-red-400", "{err}" }
            }
            if !prompt().is_empty() {
                details { class: "rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-3",
                    summary { class: "cursor-pointer text-xs font-semibold uppercase tracking-wide text-[var(--app-muted)]", "Prompt" }
                    pre { class: "mt-2 max-h-40 overflow-auto whitespace-pre-wrap font-mono text-xs text-[var(--app-fg)]", "{prompt}" }
                }
            }
            section { class: "min-h-64 flex-1 rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-3",
                h2 { class: "m-0 mb-2 text-xs font-semibold uppercase tracking-wide text-[var(--app-muted)]", "Stream" }
                pre { class: "m-0 max-h-[70vh] overflow-auto whitespace-pre-wrap break-words font-mono text-xs leading-relaxed text-[var(--app-fg)]",
                    if live().trim().is_empty() {
                        "Waiting for agent events…"
                    } else {
                        "{live}"
                    }
                }
            }
        }
    }
}
