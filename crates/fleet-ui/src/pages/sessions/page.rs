//! Chat history — saved conversations for this workspace.

use dioxus::prelude::*;

use crate::Route;
use crate::components::toast::show_toast;
use crate::pages::chat::{ConversationSummary, delete_conversation, list_conversations};

use super::api::get_session_detail;
use gloo_timers::future::TimeoutFuture;

#[component]
pub fn Sessions(workspace_id: String) -> Element {
    let mut conversations = use_signal(|| Vec::<ConversationSummary>::new());
    let mut loading = use_signal(|| true);
    let mut error = use_signal(|| Option::<String>::None);
    let navigator = use_navigator();

    let ws = workspace_id.clone();
    let mut bootstrapped = use_signal(|| false);
    use_effect(move || {
        if bootstrapped() {
            return;
        }
        bootstrapped.set(true);
        loading.set(true);
        let ws = ws.clone();
        spawn(async move {
            match list_conversations(&ws).await {
                Ok(list) => {
                    conversations.set(list);
                    error.set(None);
                }
                Err(e) => {
                    error.set(Some(e.message()));
                }
            }
            loading.set(false);
        });
    });

    rsx! {
        div { class: "flex h-full min-h-0 w-full flex-col px-4 py-6 md:px-8",
            div { class: "mb-6 flex flex-wrap items-center justify-between gap-3",
                div {
                    h1 { class: "m-0 text-xl font-semibold text-[var(--app-fg)]",
                        "Chat history"
                    }
                    p { class: "m-0 mt-1 text-sm text-[var(--app-muted)]",
                        "Saved conversations for this workspace. Open one to continue."
                    }
                }
                Link {
                    to: Route::Chat { workspace_id: workspace_id.clone() },
                    class: "inline-flex items-center rounded-xl bg-[var(--app-fg)] px-3.5 py-2 text-sm font-medium text-[var(--app-bg)] transition hover:opacity-90",
                    "New chat"
                }
            }

            if loading() {
                p { class: "text-sm text-[var(--app-muted)]", "Loading…" }
            } else if let Some(err) = error() {
                div { class: "rounded-xl border border-amber-500/30 bg-amber-500/10 px-4 py-3 text-sm text-amber-100",
                    "{err}"
                }
            } else if conversations().is_empty() {
                div {
                    class: "flex flex-col items-center justify-center gap-3 rounded-2xl border border-dashed border-[var(--app-border)] px-6 py-16 text-center",
                    p { class: "m-0 text-sm text-[var(--app-muted)]",
                        "No saved chats yet. Start a conversation and it will appear here."
                    }
                    Link {
                        to: Route::Chat { workspace_id: workspace_id.clone() },
                        class: "text-sm font-medium text-[var(--app-fg)] underline-offset-2 hover:underline",
                        "Go to Chat"
                    }
                }
            } else {
                ul { class: "m-0 flex list-none flex-col gap-2 p-0",
                    for conv in conversations() {
                        li {
                            key: "{conv.id}",
                            class: "group flex items-stretch gap-2 rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] transition hover:border-[var(--app-fg)]/20",
                            button {
                                class: "min-w-0 flex-1 cursor-pointer border-0 bg-transparent px-4 py-3 text-left",
                                r#type: "button",
                                onclick: {
                                    let ws = workspace_id.clone();
                                    let id = conv.id.clone();
                                    move |_| {
                                        navigator.push(Route::ChatThread {
                                            workspace_id: ws.clone(),
                                            conversation_id: id.clone(),
                                        });
                                    }
                                },
                                div { class: "flex items-start justify-between gap-2",
                                    h2 { class: "m-0 truncate text-sm font-semibold text-[var(--app-fg)]",
                                        "{conv.title}"
                                    }
                                    span { class: "shrink-0 text-[10px] text-[var(--app-muted)]",
                                        "{conv.message_count} msgs"
                                    }
                                }
                                if let Some(preview) = &conv.preview {
                                    p { class: "m-0 mt-1 line-clamp-2 text-xs text-[var(--app-muted)]",
                                        "{preview}"
                                    }
                                }
                                div { class: "mt-2 flex flex-wrap gap-2 text-[10px] text-[var(--app-muted)]",
                                    if let Some(p) = &conv.provider {
                                        span { class: "rounded bg-[var(--app-bg)] px-1.5 py-0.5",
                                            "{p}"
                                        }
                                    }
                                    if let Some(m) = &conv.model {
                                        span { class: "rounded bg-[var(--app-bg)] px-1.5 py-0.5 font-mono",
                                            "{m}"
                                        }
                                    }
                                    span { "{conv.updated_at}" }
                                }
                            }
                            button {
                                class: "shrink-0 self-center px-3 py-2 text-xs text-red-300/80 opacity-0 transition group-hover:opacity-100 hover:text-red-200",
                                r#type: "button",
                                title: "Delete conversation",
                                onclick: {
                                    let ws = workspace_id.clone();
                                    let id = conv.id.clone();
                                    move |_| {
                                        let ws = ws.clone();
                                        let id = id.clone();
                                        spawn(async move {
                                            match delete_conversation(&ws, &id).await {
                                                Ok(()) => {
                                                    // Reload list
                                                    match list_conversations(&ws).await {
                                                        Ok(list) => conversations.set(list),
                                                        Err(e) => show_toast().error(e.message()),
                                                    }
                                                }
                                                Err(e) => show_toast().error(e.message()),
                                            }
                                        });
                                    }
                                },
                                "Delete"
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
                            status.set(v["session"]["status"].as_str().unwrap_or("?").to_string());
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
                            if !buf.trim().is_empty() {
                                live.set(buf);
                            }
                            error.set(None);
                            let st = v["session"]["status"].as_str().unwrap_or("");
                            if matches!(
                                st,
                                "exited" | "failed" | "cancelled" | "failed_spawn" | "killing"
                            ) {
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
