//! Workspace chat — composer with provider + model dropdowns (Grok + Ollama).

use std::sync::Mutex;

use dioxus::prelude::*;
use gloo_timers::future::TimeoutFuture;
use super::api::{
    create_conversation, fetch_chat_status, fetch_fleet_live, fetch_fleet_tasks, get_conversation,
    stream_chat, ChatActionDto, ChatMessageDto, ChatModelInfo, ChatProviderInfo, ChatStatus,
    ChatStreamEvent,
};
use super::markdown::{MdPart, highlight, markdown_to_parts};
use crate::Route;
use crate::clipboard::copy_text;
use crate::components::toast::show_toast;

/// Hand off the first message across the `/chat` → `/chat/{id}` route switch.
struct PendingFirstSend {
    conversation_id: String,
    text: String,
    provider: String,
    model: String,
}

static PENDING_FIRST_SEND: Mutex<Option<PendingFirstSend>> = Mutex::new(None);

fn stash_pending_first_send(p: PendingFirstSend) {
    if let Ok(mut g) = PENDING_FIRST_SEND.lock() {
        *g = Some(p);
    }
}

fn take_pending_first_send(conversation_id: &str) -> Option<PendingFirstSend> {
    let Ok(mut g) = PENDING_FIRST_SEND.lock() else {
        return None;
    };
    match g.as_ref() {
        Some(p) if p.conversation_id == conversation_id => g.take(),
        _ => None,
    }
}

fn title_from_text(text: &str) -> String {
    let t = text.trim().replace('\n', " ");
    let chars: Vec<char> = t.chars().collect();
    if chars.is_empty() {
        return "New chat".into();
    }
    if chars.len() <= 72 {
        return t;
    }
    format!("{}…", chars.into_iter().take(69).collect::<String>())
}

#[derive(Clone, PartialEq)]
struct ChatMessage {
    id: u64,
    role: MessageRole,
    content: String,
    /// Model used for assistant turns (optional).
    model: Option<String>,
    /// Tool/sandbox actions attached to assistant replies.
    actions: Vec<ChatActionDto>,
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

fn model_option_label(m: &ChatModelInfo) -> String {
    match (&m.parameter_size, &m.family) {
        (Some(ps), Some(fam)) => format!("{} · {} · {}", m.name, fam, ps),
        (Some(ps), None) => format!("{} · {}", m.name, ps),
        (None, Some(fam)) => format!("{} · {}", m.name, fam),
        (None, None) => m.name.clone(),
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

/// Continue an existing session (already on `/chat/{id}`).
fn send_on_existing_session(
    text: String,
    workspace_id: String,
    provider: String,
    model: String,
    conversation_id: String,
    mut messages: Signal<Vec<ChatMessage>>,
    mut next_id: Signal<u64>,
    mut sending: Signal<bool>,
    mut draft: Signal<String>,
    mut thread_title: Signal<String>,
) {
    let text = text.trim().to_string();
    if text.is_empty() || sending() {
        return;
    }
    if model.trim().is_empty() {
        show_toast().error("Select a model first (Grok CLI · Ollama: `ollama pull …`).");
        return;
    }

    if messages().is_empty() {
        thread_title.set(title_from_text(&text));
    }

    let id = next_id();
    next_id.set(id + 1);
    let mut list = messages();
    list.push(ChatMessage {
        id,
        role: MessageRole::User,
        content: text.clone(),
        model: None,
        actions: vec![],
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

    {
        let mut list = messages();
        list.push(ChatMessage {
            id: reply_id,
            role: MessageRole::Assistant,
            content: String::new(),
            model: Some(model_for_api.clone()),
            actions: vec![],
        });
        messages.set(list);
    }

    spawn(async move {
        let mut streamed = String::new();
        let result = stream_chat(
            &workspace_id,
            history,
            provider,
            Some(model_for_api.clone()),
            Some(conversation_id),
            |ev| match ev {
                ChatStreamEvent::Status { text } => {
                    let mut list = messages();
                    if let Some(last) = list.iter_mut().rev().find(|m| m.id == reply_id) {
                        if last.content.is_empty() {
                            last.content = format!("_{text}_");
                        }
                    }
                    messages.set(list);
                }
                ChatStreamEvent::Delta { text } => {
                    streamed.push_str(&text);
                    let mut list = messages();
                    if let Some(last) = list.iter_mut().rev().find(|m| m.id == reply_id) {
                        last.content = streamed.clone();
                    }
                    messages.set(list);
                }
                ChatStreamEvent::Action(action) => {
                    let mut list = messages();
                    if let Some(last) = list.iter_mut().rev().find(|m| m.id == reply_id) {
                        last.actions.push(action);
                    }
                    messages.set(list);
                }
                ChatStreamEvent::Done(res) => {
                    let mut list = messages();
                    if let Some(last) = list.iter_mut().rev().find(|m| m.id == reply_id) {
                        last.content = res.message.content.clone();
                        last.model = Some(if res.model.is_empty() {
                            model_for_api.clone()
                        } else {
                            res.model.clone()
                        });
                        if !res.actions.is_empty() {
                            last.actions = res.actions.clone();
                        }
                    }
                    messages.set(list);
                }
                ChatStreamEvent::Error { error } => {
                    let mut list = messages();
                    if let Some(last) = list.iter_mut().rev().find(|m| m.id == reply_id) {
                        last.content = format!("⚠️ {error}");
                    }
                    messages.set(list);
                }
            },
        )
        .await;
        if let Err(e) = result {
            show_toast().error(e.message());
            let mut list = messages();
            if let Some(last) = list.iter_mut().rev().find(|m| m.id == reply_id) {
                if last.content.is_empty() || last.content.starts_with('_') {
                    last.content = format!("⚠️ {}", e.message());
                }
            }
            messages.set(list);
        }
        sending.set(false);
    });
}

/// First message on `/chat` (no id): create session → switch URL → continue there.
fn send_message(
    text: String,
    workspace_id: String,
    provider: String,
    model: String,
    messages: Signal<Vec<ChatMessage>>,
    next_id: Signal<u64>,
    mut sending: Signal<bool>,
    mut draft: Signal<String>,
    conversation_id: Signal<Option<String>>,
    thread_title: Signal<String>,
) {
    let navigator = navigator();
    let text = text.trim().to_string();
    if text.is_empty() || sending() {
        return;
    }
    if model.trim().is_empty() {
        show_toast().error("Select a model first (Grok CLI · Ollama: `ollama pull …`).");
        return;
    }

    if let Some(cid) = conversation_id() {
        send_on_existing_session(
            text,
            workspace_id,
            provider,
            model,
            cid,
            messages,
            next_id,
            sending,
            draft,
            thread_title,
        );
        return;
    }

    draft.set(String::new());
    sending.set(true);

    let title = title_from_text(&text);
    let provider_for_create = provider.clone();
    let model_for_create = model.clone();

    spawn(async move {
        match create_conversation(
            &workspace_id,
            Some(title),
            Some(provider_for_create.clone()),
            Some(model_for_create.clone()),
        )
        .await
        {
            Ok(conv) => {
                // Hand off first user message to the new `/chat/{id}` page.
                stash_pending_first_send(PendingFirstSend {
                    conversation_id: conv.id.clone(),
                    text: text.clone(),
                    provider: provider_for_create,
                    model: model_for_create,
                });
                navigator.replace(Route::ChatThread {
                    workspace_id: workspace_id.clone(),
                    conversation_id: conv.id,
                });
                // ChatInner remounts on the new route and drains the pending send.
            }
            Err(e) => {
                show_toast().error(e.message());
                sending.set(false);
                draft.set(text);
            }
        }
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
    // Keep selection if still listed; else first available (Grok is ordered first when ready).
    let pref = selected_provider();
    let pref_still_available = providers
        .iter()
        .any(|p| p.id == pref && p.available && !p.coming_soon);
    let provider_id = if pref_still_available {
        pref
    } else if let Some(p) = providers.iter().find(|p| p.available && !p.coming_soon) {
        p.id.clone()
    } else {
        providers
            .iter()
            .find(|p| !p.coming_soon)
            .map(|p| p.id.clone())
            .or_else(|| providers.first().map(|p| p.id.clone()))
            .unwrap_or_else(|| "grok".into())
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
    rsx! {
        ChatInner {
            workspace_id,
            initial_conversation_id: None,
        }
    }
}

#[component]
pub fn ChatThread(workspace_id: String, conversation_id: String) -> Element {
    rsx! {
        ChatInner {
            workspace_id,
            initial_conversation_id: Some(conversation_id),
        }
    }
}

#[component]
fn ChatInner(workspace_id: String, initial_conversation_id: Option<String>) -> Element {
    let mut messages = use_signal(|| Vec::<ChatMessage>::new());
    let mut draft = use_signal(|| String::new());
    let mut next_id = use_signal(|| 1u64);
    let sending = use_signal(|| false);
    let status = use_signal(|| None::<ChatStatus>);
    // Server prefers Grok when CLI auth (~/.grok/auth.json) is present.
    let mut selected_provider = use_signal(|| "grok".to_string());
    let mut selected_model = use_signal(|| String::new());
    let status_loading = use_signal(|| true);
    let mut conversation_id = use_signal(|| initial_conversation_id.clone());
    let mut loading_thread = use_signal(|| initial_conversation_id.is_some());
    let mut thread_title = use_signal(|| "New chat".to_string());
    let navigator = use_navigator();

    // Run once on mount — do NOT re-subscribe to status signals (that loops forever
    // and leaves the UI stuck on "Loading models…").
    let ws_for_status = workspace_id.clone();
    let mut status_bootstrapped = use_signal(|| false);
    use_effect(move || {
        if status_bootstrapped() {
            return;
        }
        status_bootstrapped.set(true);
        refresh_status(
            ws_for_status.clone(),
            status,
            selected_provider,
            selected_model,
            status_loading,
        );
    });

    // Open `/chat/{id}`: either drain a pending first message (just switched from /chat)
    // or load history from the server.
    let ws_load = workspace_id.clone();
    let initial_cid = initial_conversation_id.clone();
    let mut thread_bootstrapped = use_signal(|| false);
    use_effect(move || {
        if thread_bootstrapped() {
            return;
        }
        let Some(cid) = initial_cid.clone() else {
            thread_bootstrapped.set(true);
            loading_thread.set(false);
            return;
        };
        thread_bootstrapped.set(true);
        conversation_id.set(Some(cid.clone()));

        // First message just created this session — show it and request the reply.
        if let Some(pending) = take_pending_first_send(&cid) {
            loading_thread.set(false);
            thread_title.set(title_from_text(&pending.text));
            selected_provider.set(pending.provider.clone());
            selected_model.set(pending.model.clone());
            send_on_existing_session(
                pending.text,
                ws_load.clone(),
                pending.provider,
                pending.model,
                cid,
                messages,
                next_id,
                sending,
                draft,
                thread_title,
            );
            return;
        }

        loading_thread.set(true);
        let ws = ws_load.clone();
        spawn(async move {
            match get_conversation(&ws, &cid).await {
                Ok((conv, stored)) => {
                    thread_title.set(conv.title);
                    if let Some(p) = conv.provider {
                        selected_provider.set(p);
                    }
                    if let Some(m) = conv.model {
                        selected_model.set(m);
                    }
                    let mut list = Vec::new();
                    let mut nid = 1u64;
                    for m in stored {
                        let role = match m.role.as_str() {
                            "assistant" => MessageRole::Assistant,
                            _ => MessageRole::User,
                        };
                        let actions: Vec<ChatActionDto> =
                            serde_json::from_value(m.actions).unwrap_or_default();
                        list.push(ChatMessage {
                            id: nid,
                            role,
                            content: m.content,
                            model: m.model,
                            actions,
                        });
                        nid += 1;
                    }
                    next_id.set(nid);
                    messages.set(list);
                }
                Err(e) => {
                    show_toast().error(e.message());
                }
            }
            loading_thread.set(false);
        });
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
            div {
                class: "flex shrink-0 items-center justify-between gap-2 border-b border-[var(--app-border)] px-4 py-2 md:px-6",
                div { class: "min-w-0",
                    p { class: "m-0 truncate text-sm font-medium text-[var(--app-fg)]",
                        "{thread_title}"
                    }
                    if conversation_id().is_some() {
                        p { class: "m-0 text-[10px] text-[var(--app-muted)]",
                            "Saved · open Chat history for all threads"
                        }
                    }
                }
                div { class: "flex shrink-0 items-center gap-2",
                    Link {
                        to: Route::Sessions { workspace_id: workspace_id.clone() },
                        class: "rounded-lg px-2.5 py-1.5 text-xs text-[var(--app-muted)] transition hover:bg-[var(--app-bg-elevated)] hover:text-[var(--app-fg)]",
                        "History"
                    }
                    button {
                        class: "rounded-lg border border-[var(--app-border)] bg-[var(--app-bg-elevated)] px-2.5 py-1.5 text-xs font-medium text-[var(--app-fg)] transition hover:opacity-90",
                        r#type: "button",
                        onclick: {
                            let ws = workspace_id.clone();
                            move |_| {
                                navigator.push(Route::Chat { workspace_id: ws.clone() });
                            }
                        },
                        "New chat"
                    }
                }
            }

            div {
                class: "min-h-0 flex-1 overflow-y-auto px-4 pb-36 pt-6 md:px-6",
                if loading_thread() {
                    div { class: "flex h-full items-center justify-center text-sm text-[var(--app-muted)]",
                        "Loading conversation…"
                    }
                } else if messages().is_empty() {
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
                                "Say it in plain language. For implement + push, a fleet of agents runs the work — not just chat tools."
                            } else if let Some(s) = status() {
                                "{s.hint}"
                            } else {
                                "Run `grok login` and/or start local Ollama."
                            }
                        }
                        if can_chat {
                            div { class: "mt-2 flex max-w-lg flex-col gap-2",
                                ExampleChip {
                                    text: "Read the latest Slack request in #eng, implement it, push to GitHub, and write the result in a new Notion page".to_string(),
                                    draft,
                                    disabled: sending(),
                                }
                                ExampleChip {
                                    text: "What’s on that Notion page? Summarize it in Slack.".to_string(),
                                    draft,
                                    disabled: sending(),
                                }
                            }
                        }
                        if let Some(err) = status().and_then(|s| s.error.clone()) {
                            p { class: "m-0 max-w-md rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] px-3 py-2 text-left text-xs text-[var(--app-muted)]",
                                "{err}"
                            }
                        }
                    }
                } else {
                    div { class: "mx-auto flex w-full max-w-3xl flex-col gap-6",
                        for msg in messages() {
                            MessageBubble {
                                key: "{msg.id}",
                                message: msg,
                                workspace_id: workspace_id.clone(),
                            }
                        }
                        if sending() {
                            div { class: "flex items-center gap-2.5 text-sm text-[var(--app-muted)]",
                                span { class: "chat-thinking-star", "✻" }
                                span { "{thinking_label}" }
                            }
                        }
                    }
                }
            }

            div {
                class: "pointer-events-none absolute inset-x-0 bottom-0 px-4 pb-4 pt-2 md:px-6 md:pb-6",
                div { class: "pointer-events-auto mx-auto w-full max-w-3xl",
                    div {
                        class: "rounded-3xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)]",
                        textarea {
                            class: "block max-h-48 min-h-[56px] w-full resize-none bg-transparent px-4 pb-2 pt-3.5 text-[15px] leading-6 text-[var(--app-fg)] outline-none ring-0 focus:outline-none focus:ring-0 placeholder:text-[var(--app-muted)] disabled:opacity-50",
                            rows: "1",
                            placeholder: if can_chat {
                                "Write a message…"
                            } else {
                                "Grok login or Ollama required…"
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
                                                        "No chat provider ready (Grok or Ollama)"
                                                            .into()
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
                                            conversation_id,
                                            thread_title,
                                        );
                                    }
                                }
                            },
                        }

                        div {
                            class: "flex flex-wrap items-center gap-2 border-t border-[var(--app-border)]/60 px-2.5 py-2",
                            div { class: "flex min-w-0 items-center gap-1.5",
                                span {
                                    class: "hidden text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)] sm:inline",
                                    title: "Fleet agents use this same provider and model",
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
                                                    "{} — coming soon.",
                                                    p.name
                                                ));
                                                // Fall back to first available provider.
                                                if let Some(ready) = status()
                                                    .as_ref()
                                                    .and_then(|s| {
                                                        s.providers
                                                            .iter()
                                                            .find(|p| p.available && !p.coming_soon)
                                                    })
                                                {
                                                    selected_provider.set(ready.id.clone());
                                                    selected_model
                                                        .set(pick_default_model(ready));
                                                }
                                            } else if !p.available {
                                                show_toast().error(
                                                    p.error
                                                        .clone()
                                                        .unwrap_or_else(|| {
                                                            format!("{} is offline", p.name)
                                                        }),
                                                );
                                            } else {
                                                selected_model.set(pick_default_model(p));
                                            }
                                        }
                                    },
                                    if providers.is_empty() {
                                        option { value: "grok", "Grok (CLI)" }
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

                            div { class: "flex min-w-0 flex-1 items-center gap-1.5",
                                span { class: "hidden text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)] sm:inline",
                                    "Model"
                                }
                                select {
                                    class: "min-w-0 max-w-full flex-1 cursor-pointer truncate rounded-lg border-0 bg-[var(--app-bg)] px-2 py-1.5 text-xs font-medium text-[var(--app-fg)] outline-none ring-0 focus:border-0 focus:outline-none focus:ring-0 disabled:opacity-50",
                                    value: "{selected_model}",
                                    disabled: status_loading() || models.is_empty() || sending(),
                                    title: "Model for the selected provider",
                                    onchange: move |e| selected_model.set(e.value()),
                                    if models.is_empty() {
                                        option {
                                            value: "",
                                            if status_loading() {
                                                "Loading models…"
                                            } else if selected_provider() == "grok" {
                                                "Run `grok login` for grok-4.5"
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
                                                { model_option_label(m) }
                                            }
                                        }
                                    }
                                }
                            }

                            button {
                                class: "inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-lg border-0 text-[var(--app-muted)] transition hover:bg-[var(--app-bg)] hover:text-[var(--app-fg)] focus:outline-none focus:ring-0 disabled:opacity-40",
                                r#type: "button",
                                title: "Refresh providers and models",
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
                                            conversation_id,
                                            thread_title,
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
fn ExampleChip(text: String, mut draft: Signal<String>, disabled: bool) -> Element {
    let label = text.clone();
    rsx! {
        button {
            class: "rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] px-3 py-2 text-left text-xs leading-5 text-[var(--app-muted)] transition hover:border-[var(--app-fg)] hover:text-[var(--app-fg)] disabled:opacity-40",
            r#type: "button",
            disabled,
            onclick: move |_| draft.set(label.clone()),
            "{text}"
        }
    }
}

#[component]
fn MessageBubble(message: ChatMessage, workspace_id: String) -> Element {
    let is_user = message.role == MessageRole::User;
    let actions = message.actions.clone();
    let shown = if !is_user && looks_like_later_copy(&message.content) && message.actions.iter().any(|a| a.ok && is_fleet_launch(&a.tool)) {
        "Agents are working in this thread. Tool calls stay highlighted until implement → review → ship is done.".to_string()
    } else {
        message.content.clone()
    };
    let parts = if is_user {
        Vec::new()
    } else {
        markdown_to_parts(&shown)
    };
    rsx! {
        div {
            class: if is_user {
                "flex min-w-0 justify-end"
            } else {
                "min-w-0"
            },
            if is_user {
                UserBubble { text: message.content.clone() }
            } else {
                if !actions.is_empty() {
                    ToolCallGroup {
                        actions: actions.clone(),
                        workspace_id: workspace_id.clone(),
                    }
                }
                if let Some(run_id) = actions.iter().find_map(|a| {
                    if a.ok && is_fleet_launch(&a.tool) {
                        a.run_id.clone()
                    } else {
                        None
                    }
                }) {
                    AgentChatStream {
                        workspace_id: workspace_id.clone(),
                        run_id,
                    }
                }
                div { class: "flex flex-col gap-0",
                    for (i, part) in parts.iter().enumerate() {
                        match part {
                            MdPart::Html(html) => rsx! {
                                div {
                                    key: "{i}",
                                    class: "chat-md",
                                    dangerous_inner_html: "{html}",
                                }
                            },
                            MdPart::Code { lang, code } => rsx! {
                                CodeBlock {
                                    key: "{i}",
                                    lang: lang.clone(),
                                    code: code.clone(),
                                }
                            },
                        }
                    }
                }
                if let Some(model) = &message.model {
                    p { class: "m-0 mt-3 text-[11px] text-[var(--app-muted)]",
                        "{model}"
                    }
                }
            }
        }
    }
}

#[component]
fn UserBubble(text: String) -> Element {
    let mut expanded = use_signal(|| false);
    let long = text.chars().count() > 320;
    let shown = if long && !expanded() {
        let t: String = text.chars().take(280).collect();
        format!("{t}…")
    } else {
        text.clone()
    };
    rsx! {
        div { class: "max-w-[min(100%,34rem)] rounded-2xl bg-[var(--app-bg-elevated)] px-4 py-3 text-left text-[15px] leading-6 text-[var(--app-fg)]",
            p { class: "m-0 whitespace-pre-wrap", "{shown}" }
            if long {
                button {
                    class: "mt-2 text-[13px] text-[var(--app-muted)] underline-offset-2 hover:underline",
                    r#type: "button",
                    onclick: move |_| expanded.set(!expanded()),
                    if expanded() { "Show less" } else { "Show more" }
                }
            }
        }
    }
}

fn short_id(id: &str) -> String {
    id.get(..8).unwrap_or(id).to_string()
}

fn looks_like_later_copy(s: &str) -> bool {
    let t = s.to_ascii_lowercase();
    t.contains("when it finishes")
        || t.contains("when done")
        || t.contains("outputs will")
        || t.contains("they'll implement")
        || t.contains("they will implement")
        || t.contains("say continue")
}

fn tool_used_label(raw: &str) -> String {
    let t = raw
        .trim()
        .trim_start_matches("Using ")
        .trim_start_matches("using ")
        .trim_end_matches('…')
        .trim_end_matches("...");
    format!("Used {t}")
}

fn clip_live_text(raw: &str) -> Option<String> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    if lower.contains("min-height")
        || lower.contains("box-sizing")
        || (t.matches('{').count() >= 2 && t.contains(';'))
    {
        return None;
    }
    if t.starts_with('{') && t.contains("\"ok\"") && t.len() > 180 {
        return Some("got a tool result".into());
    }
    let one = t.replace('\n', " ");
    let chars: Vec<char> = one.chars().collect();
    if chars.len() > 140 {
        Some(format!("{}…", chars.into_iter().take(137).collect::<String>()))
    } else {
        Some(one)
    }
}

fn status_tone(status: &str) -> &'static str {
    match status {
        "done_success" | "completed" | "healthy" => "text-emerald-400",
        "in_progress" | "assigned" | "spawning" | "active" => "text-sky-400",
        "queued" | "ready" | "retry_wait" => "text-amber-400",
        "done_failed" | "failed" | "failed_spawn" => "text-red-400",
        _ => "text-[var(--app-muted)]",
    }
}

fn is_terminal_run(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "cancelled")
}

fn agent_phase(status: &str) -> String {
    match status {
        "in_progress" | "assigned" | "spawning" | "healthy" => "working".into(),
        "queued" | "ready" | "retry_wait" => "queued".into(),
        "done_success" | "completed" => "done".into(),
        "done_failed" | "failed" | "failed_spawn" => "failed".into(),
        other if other.is_empty() => "queued".into(),
        other => other.to_string(),
    }
}

#[component]
fn AgentChatStream(workspace_id: String, run_id: String) -> Element {
    let mut tasks = use_signal(Vec::<(String, String, String, String)>::new);
    let mut activity = use_signal(Vec::<(String, String, String)>::new);
    let mut chat = use_signal(Vec::<(String, String, String)>::new);
    let mut run_status = use_signal(|| "active".to_string());
    let mut live = use_signal(|| 0u64);
    let mut err = use_signal(|| None::<String>);

    let ws = workspace_id.clone();
    let rid = run_id.clone();
    use_future(move || {
        let ws = ws.clone();
        let rid = rid.clone();
        async move {
            loop {
                match fetch_fleet_live(&ws).await {
                    Ok(o) => {
                        let mut st = run_status();
                        if let Some(r) = o
                            .runs
                            .iter()
                            .find(|r| r.get("id").and_then(|x| x.as_str()) == Some(rid.as_str()))
                        {
                            st = r
                                .get("status")
                                .and_then(|s| s.as_str())
                                .unwrap_or("active")
                                .to_string();
                            run_status.set(st.clone());
                        }
                        live.set(o.live_agents);
                        let mut board = o.tasks.clone();
                        if let Ok(all) = fetch_fleet_tasks(&ws).await {
                            if !all.tasks.is_empty() {
                                board = all.tasks;
                            }
                        }
                        let ts: Vec<(String, String, String, String)> = board
                            .iter()
                            .filter(|t| {
                                t.get("run_id")
                                    .and_then(|x| x.as_str())
                                    .map(|id| id == rid || id.starts_with(&rid) || rid.starts_with(id))
                                    .unwrap_or(false)
                            })
                            .filter_map(|t| {
                                Some((
                                    t.get("plan_task_id")?.as_str()?.to_string(),
                                    t.get("title")?.as_str()?.to_string(),
                                    t.get("worker_profile")
                                        .and_then(|x| x.as_str())
                                        .unwrap_or("")
                                        .to_string(),
                                    t.get("status")?.as_str()?.to_string(),
                                ))
                            })
                            .collect();
                        tasks.set(ts);
                        activity.set(
                            o.activity
                                .iter()
                                .filter_map(|a| {
                                    let kind = a.get("kind")?.as_str()?.to_string();
                                    if kind == "tool_result" {
                                        return None;
                                    }
                                    let text = clip_live_text(a.get("text")?.as_str()?)?;
                                    Some((
                                        a.get("profile")?.as_str()?.to_string(),
                                        kind,
                                        text,
                                    ))
                                })
                                .take(24)
                                .collect(),
                        );
                        chat.set(
                            o.chat
                                .iter()
                                .rev()
                                .filter_map(|m| {
                                    Some((
                                        m.get("from")?.as_str()?.to_string(),
                                        m.get("to").and_then(|x| x.as_str()).unwrap_or("*").to_string(),
                                        m.get("body")?.as_str()?.to_string(),
                                    ))
                                })
                                .take(6)
                                .collect::<Vec<_>>()
                                .into_iter()
                                .rev()
                                .collect(),
                        );
                        err.set(None);
                        if is_terminal_run(&st) {
                            break;
                        }
                    }
                    Err(e) => err.set(Some(e.message())),
                }
                TimeoutFuture::new(2000).await;
            }
        }
    });

    let done = is_terminal_run(&run_status());
    let _ = live;
    let _ = workspace_id;
    rsx! {
        div { class: "mt-5 flex flex-col gap-3",
            // Who is working / queued — a line in the thread, not a board.
            div { class: "flex flex-wrap gap-x-3 gap-y-1 text-[13px]",
                for (id, title, profile, status) in tasks() {
                    {
                        let who = if profile.is_empty() { title.clone() } else { profile.clone() };
                        let phase = agent_phase(&status);
                        rsx! {
                            span { class: "text-[var(--app-muted)]",
                                span { class: "font-medium text-[var(--app-fg)]", "{id}" }
                                " "
                                span { class: "{status_tone(&status)}", "{phase}" }
                                span { class: "opacity-50", " · {who}" }
                            }
                        }
                    }
                }
                if tasks().is_empty() && !done {
                    span { class: "text-[var(--app-muted)]", "starting agents…" }
                }
                if done {
                    span { class: "text-emerald-400", "all shipped" }
                }
            }
            if let Some(e) = err() {
                p { class: "m-0 text-xs text-red-400", "{e}" }
            }
            // Grok-style tool / think lines in the same chat column.
            div { class: "flex flex-col gap-2.5",
                for (i, (profile, kind, text)) in activity().into_iter().enumerate() {
                    {
                        let is_tool = kind == "tool" || kind.starts_with("use_");
                        let is_think = kind == "think" || kind == "start";
                        rsx! {
                            div { key: "{i}", class: "min-w-0",
                                p { class: "m-0 mb-0.5 text-[11px] text-[var(--app-muted)]",
                                    "{profile}"
                                }
                                if is_tool {
                                    div { class: "chat-tool-rail pl-3 text-[13px] text-[var(--app-muted)]",
                                        span { class: "font-medium text-[var(--app-fg)]", "{tool_used_label(&text)}" }
                                    }
                                } else if is_think {
                                    p { class: "m-0 text-[15px] leading-6 text-[var(--app-fg)]",
                                        "{text}"
                                    }
                                } else {
                                    p { class: "m-0 text-[13px] leading-5 text-[var(--app-muted)]",
                                        "{text}"
                                    }
                                }
                            }
                        }
                    }
                }
            }
            for (from, _to, body) in chat() {
                p { class: "m-0 text-[13px] leading-5 text-[var(--app-muted)]",
                    span { class: "font-medium text-[var(--app-fg)]", "{from}" }
                    " {body}"
                }
            }
        }
    }
}

fn action_url(action: &ChatActionDto) -> Option<String> {
    let d = action.detail.as_ref()?;
    d.get("url")
        .or_else(|| d.pointer("/data/url"))
        .and_then(|v| v.as_str())
        .filter(|s| s.starts_with("http"))
        .map(|s| s.to_string())
}

fn is_fleet_launch(tool: &str) -> bool {
    tool == "launch_agent_plan" || tool == "launch_agent_task"
}

/// Claude-style collapsible tool rail (clock + check, muted body).
#[component]
fn ToolCallGroup(actions: Vec<ChatActionDto>, workspace_id: String) -> Element {
    let _ = workspace_id;
    let mut open = use_signal(|| true);
    let all_ok = actions.iter().all(|a| a.ok);
    let fleet = actions.iter().find(|a| a.ok && is_fleet_launch(&a.tool));
    let n_agents = fleet
        .and_then(|a| a.sandbox_ids.as_ref())
        .map(|ids| ids.len())
        .unwrap_or(0);
    let title = if let Some(f) = fleet {
        if n_agents > 0 {
            format!("Fleet · {n_agents} agents")
        } else {
            f.summary.clone()
        }
    } else if actions.len() == 1 {
        let a = &actions[0];
        if a.summary.is_empty() {
            a.tool.clone()
        } else {
            a.summary.clone()
        }
    } else {
        format!("Used {} tools", actions.len())
    };
    rsx! {
        div { class: "mb-4",
            button {
                class: "flex w-full items-center gap-2 py-0.5 text-left text-[13px] text-[var(--app-muted)] transition hover:text-[var(--app-fg)]",
                r#type: "button",
                onclick: move |_| open.set(!open()),
                span { class: "min-w-0 flex-1 truncate", "{title}" }
                span { class: "shrink-0 text-[11px] opacity-60",
                    if open() { "▾" } else { "▸" }
                }
            }
            if open() {
                div { class: "chat-tool-rail mt-1.5 pl-3",
                    for (i, action) in actions.iter().enumerate() {
                        {
                            let extra = action.run_id.as_ref().map(|id| format!("run {}", short_id(id)));
                            let boxes = action.sandbox_ids.as_ref().and_then(|ids| {
                                if ids.is_empty() {
                                    None
                                } else {
                                    Some(format!(
                                        "{} agent{}",
                                        ids.len(),
                                        if ids.len() == 1 { "" } else { "s" }
                                    ))
                                }
                            });
                            let href = action_url(action);
                            rsx! {
                                div {
                                    key: "{i}",
                                    class: "mb-2.5 last:mb-1",
                                    div { class: "flex items-start gap-2 text-[13px] leading-5 text-[var(--app-muted)]",
                                        span { class: "chat-tool-clock mt-0.5 shrink-0",
                                            if action.ok { "✓" } else { "!" }
                                        }
                                        div { class: "min-w-0",
                                            p { class: "m-0 font-medium text-[var(--app-fg)]",
                                                if is_fleet_launch(&action.tool) {
                                                    "Fleet launch"
                                                } else {
                                                    "{action.tool}"
                                                }
                                            }
                                            if !action.summary.is_empty() {
                                                p { class: "m-0 mt-0.5", "{action.summary}" }
                                            }
                                            if let Some(url) = href {
                                                a {
                                                    class: "mt-0.5 block truncate text-[12px] underline-offset-2 hover:underline",
                                                    href: "{url}",
                                                    target: "_blank",
                                                    rel: "noreferrer",
                                                    "{url}"
                                                }
                                            }
                                            if let Some(line) = &extra {
                                                p { class: "m-0 mt-0.5 font-mono text-[11px] opacity-80",
                                                    "{line}"
                                                }
                                            }
                                            if let Some(line) = &boxes {
                                                p { class: "m-0 mt-0.5 font-mono text-[11px] opacity-80",
                                                    "{line}"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    div { class: "mt-1 flex items-center gap-2 text-[13px] text-[var(--app-muted)]",
                        span {
                            class: if all_ok {
                                "chat-tool-check"
                            } else {
                                "chat-tool-fail"
                            },
                            if all_ok { "✓" } else { "○" }
                        }
                        span { if all_ok { "Done" } else { "Failed" } }
                    }
                }
            }
        }
    }
}

#[component]
fn CodeBlock(lang: String, code: String) -> Element {
    let mut copied = use_signal(|| false);
    let highlighted = highlight(&lang, &code);
    let label = if lang.is_empty() {
        String::new()
    } else {
        lang.clone()
    };
    let code_for_copy = code.clone();
    rsx! {
        div { class: "chat-code my-3",
            div { class: "chat-code-bar",
                span { class: "chat-code-lang",
                    if label.is_empty() { "code" } else { "{label}" }
                }
                button {
                    class: "chat-code-copy",
                    r#type: "button",
                    title: "Copy",
                    "aria-label": "Copy code",
                    onclick: move |_| {
                        if copy_text(&code_for_copy) {
                            copied.set(true);
                        } else {
                            show_toast().error("Copy failed. Select the code and copy it yourself.");
                        }
                    },
                    if copied() { "Copied" } else { "Copy" }
                }
            }
            pre {
                class: "chat-code-pre",
                dangerous_inner_html: "{highlighted}",
            }
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
