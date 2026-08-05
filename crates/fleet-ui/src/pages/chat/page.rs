//! Workspace chat — composer with provider + model dropdowns (live Ollama list).

use dioxus::prelude::*;

use super::api::{
    complete_chat, fetch_chat_status, ChatMessageDto, ChatModelInfo, ChatProviderInfo, ChatStatus,
};
use crate::components::toast::show_toast;

#[derive(Clone, PartialEq)]
struct ChatMessage {
    id: u64,
    role: MessageRole,
    content: String,
    /// Model used for assistant turns (optional).
    model: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MessageRole {
    User,
    Assistant,
}

impl MessageRole {
    fn as_api(&self) -> &'static str {
        match self {
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
        }
    }
}

fn pick_default_model(provider: &ChatProviderInfo) -> String {
    if let Some(d) = &provider.default_model {
        // Match exact or tag prefix (env `llama3.2` vs list `llama3.2:latest`).
        if provider.models.iter().any(|m| m.name == *d) {
            return d.clone();
        }
        if let Some(m) = provider
            .models
            .iter()
            .find(|m| m.name.starts_with(&format!("{d}:")) || m.name.starts_with(d))
        {
            return m.name.clone();
        }
    }
    provider
        .models
        .first()
        .map(|m| m.name.clone())
        .unwrap_or_default()
}

fn send_message(
    text: String,
    workspace_id: String,
    provider: String,
    model: String,
    mut messages: Signal<Vec<ChatMessage>>,
    mut next_id: Signal<u64>,
    mut sending: Signal<bool>,
    mut draft: Signal<String>,
) {
    let text = text.trim().to_string();
    if text.is_empty() || sending() {
        return;
    }
    if model.trim().is_empty() {
        show_toast().error("Select a model first (pull one with `ollama pull …`).");
        return;
    }

    let id = next_id();
    next_id.set(id + 1);
    let mut list = messages();
    list.push(ChatMessage {
        id,
        role: MessageRole::User,
        content: text,
        model: None,
    });
    messages.set(list);
    draft.set(String::new());
    sending.set(true);

    let history: Vec<ChatMessageDto> = messages()
        .iter()
        .map(|m| ChatMessageDto {
            role: m.role.as_api().to_string(),
            content: m.content.clone(),
        })
        .collect();

    let model_for_api = model.clone();
    let reply_id = {
        let rid = next_id();
        next_id.set(rid + 1);
        rid
    };

    spawn(async move {
        match complete_chat(
            &workspace_id,
            history,
            provider,
            Some(model_for_api.clone()),
        )
        .await
        {
            Ok(res) => {
                let mut list = messages();
                list.push(ChatMessage {
                    id: reply_id,
                    role: MessageRole::Assistant,
                    content: res.message.content,
                    model: Some(if res.model.is_empty() {
                        model_for_api
                    } else {
                        res.model
                    }),
                });
                messages.set(list);
            }
            Err(e) => {
                show_toast().error(e.message());
                let mut list = messages();
                list.push(ChatMessage {
                    id: reply_id,
                    role: MessageRole::Assistant,
                    content: format!("⚠️ {}", e.message()),
                    model: Some(model_for_api),
                });
                messages.set(list);
            }
        }
        sending.set(false);
    });
}

fn refresh_status(
    workspace_id: String,
    status: Signal<Option<ChatStatus>>,
    selected_provider: Signal<String>,
    selected_model: Signal<String>,
    mut status_loading: Signal<bool>,
) {
    status_loading.set(true);
    spawn(async move {
        match fetch_chat_status(&workspace_id).await {
            Ok(s) => apply_status(s, status, selected_provider, selected_model),
            Err(e) => {
                show_toast().error(format!("Chat status: {}", e.message()));
                let mut status = status;
                status.set(None);
            }
        }
        status_loading.set(false);
    });
}

fn apply_status(
    s: ChatStatus,
    mut status: Signal<Option<ChatStatus>>,
    mut selected_provider: Signal<String>,
    mut selected_model: Signal<String>,
) {
    let providers = &s.providers;
    // Prefer previously selected provider if still present, else first available, else ollama.
    let pref = selected_provider();
    let provider_id = if providers.iter().any(|p| p.id == pref) {
        pref
    } else if let Some(p) = providers.iter().find(|p| p.available) {
        p.id.clone()
    } else {
        providers
            .first()
            .map(|p| p.id.clone())
            .unwrap_or_else(|| "ollama".into())
    };
    selected_provider.set(provider_id.clone());

    let models = providers
        .iter()
        .find(|p| p.id == provider_id)
        .map(|p| p.models.clone())
        .unwrap_or_default();

    let current_model = selected_model();
    if current_model.is_empty() || !models.iter().any(|m| m.name == current_model) {
        let pick = providers
            .iter()
            .find(|p| p.id == provider_id)
            .map(pick_default_model)
            .unwrap_or_default();
        selected_model.set(pick);
    }

    status.set(Some(s));
}

#[component]
pub fn Chat(workspace_id: String) -> Element {
    let messages = use_signal(|| Vec::<ChatMessage>::new());
    let mut draft = use_signal(|| String::new());
    let next_id = use_signal(|| 1u64);
    let sending = use_signal(|| false);
    let status = use_signal(|| None::<ChatStatus>);
    let mut selected_provider = use_signal(|| "ollama".to_string());
    let mut selected_model = use_signal(|| String::new());
    let status_loading = use_signal(|| true);

    let ws_for_status = workspace_id.clone();
    use_effect(move || {
        refresh_status(
            ws_for_status.clone(),
            status,
            selected_provider,
            selected_model,
            status_loading,
        );
    });

    let providers: Vec<ChatProviderInfo> = status()
        .as_ref()
        .map(|s| s.providers.clone())
        .unwrap_or_default();

    let active_provider = providers
        .iter()
        .find(|p| p.id == selected_provider())
        .cloned();

    let models: Vec<ChatModelInfo> = active_provider
        .as_ref()
        .map(|p| p.models.clone())
        .unwrap_or_default();

    let can_chat = active_provider
        .as_ref()
        .map(|p| p.available && !p.models.is_empty())
        .unwrap_or(false);

    let thinking_label = {
        let m = selected_model();
        if m.is_empty() {
            "Thinking…".to_string()
        } else {
            format!("Thinking with {m}…")
        }
    };

    rsx! {
        div { class: "relative flex h-full min-h-0 w-full flex-col",
            // Messages
            div {
                class: "min-h-0 flex-1 overflow-y-auto px-4 pb-36 pt-6 md:px-6",
                if messages().is_empty() {
                    div { class: "flex h-full flex-col items-center justify-center gap-3 text-center",
                        div {
                            class: "flex h-12 w-12 items-center justify-center rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] text-[var(--app-muted)]",
                            ChatBubbleIcon {}
                        }
                        h2 { class: "m-0 text-lg font-semibold text-[var(--app-fg)]",
                            "How can I help?"
                        }
                        p { class: "m-0 max-w-md text-sm text-[var(--app-muted)]",
                            if can_chat {
                                "Pick a provider and model below, then send a message. Ollama models are loaded live from your machine (`ollama list`)."
                            } else if let Some(s) = status() {
                                "{s.hint}"
                            } else {
                                "Connect local Ollama to chat with models on your machine."
                            }
                        }
                        if let Some(err) = status().and_then(|s| s.error.clone()) {
                            p { class: "m-0 max-w-md rounded-xl border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-left text-xs text-amber-200",
                                "{err}"
                            }
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
                                    "{thinking_label}"
                                }
                            }
                        }
                    }
                }
            }

            // Composer with provider + model dropdowns inside the card
            div {
                class: "pointer-events-none absolute inset-x-0 bottom-0 px-4 pb-4 pt-2 md:px-6 md:pb-6",
                div { class: "pointer-events-auto mx-auto w-full max-w-3xl",
                    div {
                        class: "rounded-3xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)]",
                        textarea {
                            class: "block max-h-48 min-h-[56px] w-full resize-none bg-transparent px-4 pb-2 pt-3.5 text-[15px] leading-6 text-[var(--app-fg)] outline-none ring-0 focus:outline-none focus:ring-0 placeholder:text-[var(--app-muted)] disabled:opacity-50",
                            rows: "1",
                            placeholder: if can_chat {
                                "Message the fleet…"
                            } else {
                                "Start Ollama and pick a model…"
                            },
                            value: "{draft}",
                            disabled: !can_chat || sending(),
                            oninput: move |e| draft.set(e.value()),
                            onkeydown: {
                                let ws = workspace_id.clone();
                                move |e| {
                                    if e.key() == Key::Enter && !e.modifiers().shift() {
                                        e.prevent_default();
                                        if !can_chat {
                                            show_toast().error(
                                                status()
                                                    .and_then(|s| s.error.clone())
                                                    .unwrap_or_else(|| {
                                                        "Ollama is offline or has no models".into()
                                                    }),
                                            );
                                            return;
                                        }
                                        send_message(
                                            draft(),
                                            ws.clone(),
                                            selected_provider(),
                                            selected_model(),
                                            messages,
                                            next_id,
                                            sending,
                                            draft,
                                        );
                                    }
                                }
                            },
                        }

                        // Toolbar: Provider | Model | refresh ………… send
                        div {
                            class: "flex flex-wrap items-center gap-2 border-t border-[var(--app-border)]/60 px-2.5 py-2",
                            // Provider
                            div { class: "flex min-w-0 items-center gap-1.5",
                                span { class: "hidden text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)] sm:inline",
                                    "Provider"
                                }
                                select {
                                    class: "max-w-[10rem] cursor-pointer truncate rounded-lg border-0 bg-[var(--app-bg)] px-2 py-1.5 text-xs font-medium text-[var(--app-fg)] outline-none ring-0 focus:border-0 focus:outline-none focus:ring-0 disabled:opacity-50",
                                    value: "{selected_provider}",
                                    disabled: status_loading() || providers.is_empty(),
                                    title: "Chat provider",
                                    onchange: move |e| {
                                        let id = e.value();
                                        selected_provider.set(id.clone());
                                        if let Some(p) = status()
                                            .as_ref()
                                            .and_then(|s| s.providers.iter().find(|p| p.id == id))
                                        {
                                            if p.coming_soon {
                                                show_toast().info(format!(
                                                    "{} — coming soon. Using Ollama for now.",
                                                    p.name
                                                ));
                                                selected_provider.set("ollama".into());
                                                if let Some(ollama) = status()
                                                    .as_ref()
                                                    .and_then(|s| s.providers.iter().find(|p| p.id == "ollama"))
                                                {
                                                    selected_model.set(pick_default_model(ollama));
                                                }
                                            } else {
                                                selected_model.set(pick_default_model(p));
                                            }
                                        }
                                    },
                                    if providers.is_empty() {
                                        option { value: "ollama", "Ollama (local)" }
                                    } else {
                                        for p in providers.iter() {
                                            option {
                                                key: "{p.id}",
                                                value: "{p.id}",
                                                selected: selected_provider() == p.id,
                                                disabled: p.coming_soon,
                                                if p.coming_soon {
                                                    "{p.name} · soon"
                                                } else if p.available {
                                                    "{p.name}"
                                                } else {
                                                    "{p.name} · offline"
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            // Model (live Ollama list)
                            div { class: "flex min-w-0 flex-1 items-center gap-1.5",
                                span { class: "hidden text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)] sm:inline",
                                    "Model"
                                }
                                select {
                                    class: "min-w-0 max-w-full flex-1 cursor-pointer truncate rounded-lg border-0 bg-[var(--app-bg)] px-2 py-1.5 text-xs font-medium text-[var(--app-fg)] outline-none ring-0 focus:border-0 focus:outline-none focus:ring-0 disabled:opacity-50",
                                    value: "{selected_model}",
                                    disabled: status_loading() || models.is_empty() || sending(),
                                    title: "Model from ollama list",
                                    onchange: move |e| selected_model.set(e.value()),
                                    if models.is_empty() {
                                        option {
                                            value: "",
                                            if status_loading() {
                                                "Loading models…"
                                            } else {
                                                "No models — ollama pull …"
                                            }
                                        }
                                    } else {
                                        for m in models.iter() {
                                            option {
                                                key: "{m.name}",
                                                value: "{m.name}",
                                                selected: selected_model() == m.name,
                                                {
                                                    match (&m.parameter_size, &m.family) {
                                                        (Some(ps), Some(fam)) => format!("{} · {} · {}", m.name, fam, ps),
                                                        (Some(ps), None) => format!("{} · {}", m.name, ps),
                                                        (None, Some(fam)) => format!("{} · {}", m.name, fam),
                                                        (None, None) => m.name.clone(),
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            // Refresh models from Ollama
                            button {
                                class: "inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border-0 text-[var(--app-muted)] transition hover:bg-[var(--app-bg)] hover:text-[var(--app-fg)] focus:outline-none focus:ring-0 disabled:opacity-40",
                                r#type: "button",
                                title: "Refresh model list from Ollama",
                                "aria-label": "Refresh models",
                                disabled: status_loading(),
                                onclick: {
                                    let ws = workspace_id.clone();
                                    move |_| {
                                        refresh_status(
                                            ws.clone(),
                                            status,
                                            selected_provider,
                                            selected_model,
                                            status_loading,
                                        );
                                    }
                                },
                                RefreshIcon {}
                            }

                            // Send
                            button {
                                class: "ml-auto inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-full bg-[var(--app-fg)] text-[var(--app-bg)] transition hover:opacity-90 focus:outline-none focus:ring-0 disabled:cursor-not-allowed disabled:opacity-30",
                                r#type: "button",
                                title: "Send message",
                                "aria-label": "Send message",
                                disabled: draft().trim().is_empty() || sending() || !can_chat,
                                onclick: {
                                    let ws = workspace_id.clone();
                                    move |_| {
                                        send_message(
                                            draft(),
                                            ws.clone(),
                                            selected_provider(),
                                            selected_model(),
                                            messages,
                                            next_id,
                                            sending,
                                            draft,
                                        );
                                    }
                                },
                                SendIcon {}
                            }
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
            div { class: "flex max-w-[85%] flex-col gap-1",
                div {
                    class: if is_user {
                        "whitespace-pre-wrap rounded-2xl rounded-br-md bg-[var(--app-fg)] px-3.5 py-2.5 text-sm leading-relaxed text-[var(--app-bg)]"
                    } else {
                        "whitespace-pre-wrap rounded-2xl rounded-bl-md border border-[var(--app-border)] bg-[var(--app-bg-elevated)] px-3.5 py-2.5 text-sm leading-relaxed text-[var(--app-fg)]"
                    },
                    "{message.content}"
                }
                if let Some(model) = &message.model {
                    if !is_user {
                        span { class: "px-1 text-[10px] text-[var(--app-muted)]",
                            "{model}"
                        }
                    }
                }
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
fn RefreshIcon() -> Element {
    rsx! {
        svg {
            class: "h-3.5 w-3.5",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            path { d: "M21 12a9 9 0 1 1-2.64-6.36" }
            path { d: "M21 3v6h-6" }
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
