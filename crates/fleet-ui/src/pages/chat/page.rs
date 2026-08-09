//! Workspace chat — messages feed + bottom composer (AI-style input).

use dioxus::prelude::*;

#[derive(Clone, PartialEq)]
struct ChatMessage {
    id: u64,
    role: MessageRole,
    content: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MessageRole {
    User,
    Assistant,
}

fn push_user_and_reply(
    text: String,
    mut messages: Signal<Vec<ChatMessage>>,
    mut next_id: Signal<u64>,
    mut sending: Signal<bool>,
    mut draft: Signal<String>,
) {
    let text = text.trim().to_string();
    if text.is_empty() || sending() {
        return;
    }

    let id = next_id();
    next_id.set(id + 1);
    let mut list = messages();
    list.push(ChatMessage {
        id,
        role: MessageRole::User,
        content: text.clone(),
    });
    messages.set(list);
    draft.set(String::new());
    sending.set(true);

    // Placeholder assistant reply until a real chat API is wired.
    let reply_id = id + 1;
    next_id.set(reply_id + 1);
    spawn(async move {
        #[cfg(target_arch = "wasm32")]
        {
            gloo_timers::future::TimeoutFuture::new(450).await;
        }
        let mut list = messages();
        list.push(ChatMessage {
            id: reply_id,
            role: MessageRole::Assistant,
            content: format!(
                "Got it — I'll work with the fleet on: \"{}\"\n\n(Chat backend not connected yet.)",
                text
            ),
        });
        messages.set(list);
        sending.set(false);
    });
}

#[component]
pub fn Chat(workspace_id: String) -> Element {
    let _ = &workspace_id;

    let mut messages = use_signal(|| Vec::<ChatMessage>::new());
    let mut draft = use_signal(|| String::new());
    let next_id = use_signal(|| 1u64);
    let mut sending = use_signal(|| false);

    rsx! {
        div { class: "relative flex h-full min-h-0 w-full flex-col",
            // Messages
            div {
                class: "min-h-0 flex-1 overflow-y-auto px-4 pb-28 pt-6 md:px-6",
                if messages().is_empty() {
                    div { class: "flex h-full flex-col items-center justify-center gap-3 text-center",
                        div {
                            class: "flex h-12 w-12 items-center justify-center rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] text-[var(--app-muted)]",
                            ChatBubbleIcon {}
                        }
                        h2 { class: "m-0 text-lg font-semibold text-[var(--app-fg)]",
                            "How can I help?"
                        }
                        p { class: "m-0 max-w-sm text-sm text-[var(--app-muted)]",
                            "Ask the fleet to plan work, check status, or kick off a run. Type below to start."
                        }
                    }
                } else {
                    div { class: "mx-auto flex w-full max-w-3xl flex-col gap-4",
                        for msg in messages() {
                            MessageBubble { key: "{msg.id}", message: msg }
                        }
                        if sending() {
                            div { class: "flex items-start gap-3",
                                Avatar { role: MessageRole::Assistant }
                                div {
                                    class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] px-3.5 py-2.5 text-sm text-[var(--app-muted)]",
                                    "Thinking…"
                                }
                            }
                        }
                    }
                }
            }

            // Floating composer — ChatGPT / Claude style (no top bar, no separator)
            div {
                class: "pointer-events-none absolute inset-x-0 bottom-0 px-4 pb-4 pt-2 md:px-6 md:pb-6",
                div { class: "pointer-events-auto mx-auto w-full max-w-3xl",
                    div {
                        class: "relative rounded-3xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] focus-within:border-[var(--app-muted)]",
                        textarea {
                            class: "block max-h-48 min-h-[56px] w-full resize-none bg-transparent px-4 pb-12 pt-3.5 text-[15px] leading-6 text-[var(--app-fg)] outline-none placeholder:text-[var(--app-muted)]",
                            rows: "1",
                            placeholder: "Message the fleet…",
                            value: "{draft}",
                            oninput: move |e| draft.set(e.value()),
                            onkeydown: move |e| {
                                // Enter sends; Shift+Enter inserts a newline.
                                if e.key() == Key::Enter && !e.modifiers().shift() {
                                    e.prevent_default();
                                    push_user_and_reply(
                                        draft(),
                                        messages,
                                        next_id,
                                        sending,
                                        draft,
                                    );
                                }
                            },
                        }
                        // Send sits bottom-right inside the card (standard AI composer)
                        button {
                            class: "absolute bottom-2.5 right-2.5 inline-flex h-8 w-8 items-center justify-center rounded-full bg-[var(--app-fg)] text-[var(--app-bg)] transition hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-30",
                            r#type: "button",
                            title: "Send message",
                            "aria-label": "Send message",
                            disabled: draft().trim().is_empty() || sending(),
                            onclick: move |_| {
                                push_user_and_reply(
                                    draft(),
                                    messages,
                                    next_id,
                                    sending,
                                    draft,
                                );
                            },
                            SendIcon {}
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn MessageBubble(message: ChatMessage) -> Element {
    let is_user = message.role == MessageRole::User;
    rsx! {
        div {
            class: if is_user {
                "flex items-start justify-end gap-3"
            } else {
                "flex items-start gap-3"
            },
            if !is_user {
                Avatar { role: MessageRole::Assistant }
            }
            div {
                class: if is_user {
                    "max-w-[85%] whitespace-pre-wrap rounded-2xl rounded-br-md bg-[var(--app-fg)] px-3.5 py-2.5 text-sm leading-relaxed text-[var(--app-bg)]"
                } else {
                    "max-w-[85%] whitespace-pre-wrap rounded-2xl rounded-bl-md border border-[var(--app-border)] bg-[var(--app-bg-elevated)] px-3.5 py-2.5 text-sm leading-relaxed text-[var(--app-fg)]"
                },
                "{message.content}"
            }
            if is_user {
                Avatar { role: MessageRole::User }
            }
        }
    }
}

#[component]
fn Avatar(role: MessageRole) -> Element {
    let label = match role {
        MessageRole::User => "You",
        MessageRole::Assistant => "AF",
    };
    rsx! {
        div {
            class: match role {
                MessageRole::User => "flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-[var(--app-bg-elevated)] text-[10px] font-semibold text-[var(--app-muted)] ring-1 ring-[var(--app-border)]",
                MessageRole::Assistant => "flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-[var(--app-accent)] text-[10px] font-semibold text-white",
            },
            "{label}"
        }
    }
}

#[component]
fn SendIcon() -> Element {
    rsx! {
        svg {
            class: "h-4 w-4",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            path { d: "M22 2 11 13" }
            path { d: "M22 2 15 22 11 13 2 9z" }
        }
    }
}

#[component]
fn ChatBubbleIcon() -> Element {
    rsx! {
        svg {
            class: "h-5 w-5",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.75",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            path { d: "M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" }
        }
    }
}
