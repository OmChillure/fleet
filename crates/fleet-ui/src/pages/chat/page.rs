//! Workspace chat — composer with provider + model dropdowns (Grok + Ollama).

use std::sync::Mutex;

use super::api::{
    create_conversation, fetch_chat_status, fetch_fleet_live, fetch_fleet_tasks, get_conversation,
    stream_chat, ChatActionDto, ChatMessageDto, ChatModelInfo, ChatProviderInfo, ChatStatus,
    ChatStreamEvent,
};
use super::markdown::{highlight, markdown_to_parts, MdPart};
use crate::clipboard::copy_text;
use crate::components::toast::show_toast;
use crate::pages::runners::self_hosted::api::{list_workers_page, HostPublic};
use crate::Route;
use dioxus::prelude::*;
use fleet_runners_view::{
    agent_status_phrase, catalog_provider_display_name, claude_cli_model_aliases,
    claude_effort_values, display_name, find_model_detail, format_chat_elapsed, is_removed,
    pick_chat_provider_and_model, pick_effort_value,
    should_apply_chat_defaults, ChatProviderCandidate, ChatProviderPick,
};
use gloo_timers::future::TimeoutFuture;

/// Hand off the first message across the `/chat` → `/chat/{id}` route switch.
struct PendingFirstSend {
    conversation_id: String,
    text: String,
    provider: String,
    model: String,
    runner_id: String,
    effort: String,
}

static PENDING_FIRST_SEND: Mutex<Option<PendingFirstSend>> = Mutex::new(None);

fn stash_pending_first_send(p: PendingFirstSend) {
    if let Ok(mut g) = PENDING_FIRST_SEND.lock() {
        *g = Some(p);
    }
}

fn peek_pending_runner(conversation_id: Option<&str>) -> Option<String> {
    let cid = conversation_id?;
    let Ok(g) = PENDING_FIRST_SEND.lock() else {
        return None;
    };
    g.as_ref()
        .filter(|p| p.conversation_id == cid)
        .map(|p| p.runner_id.clone())
        .filter(|s| !s.is_empty())
}

fn storage_pref_key(kind: &str, workspace_id: &str) -> String {
    format!("fleet-chat-{kind}:{workspace_id}")
}

fn read_stored_pref(kind: &str, workspace_id: &str) -> Option<String> {
    let key = storage_pref_key(kind, workspace_id);
    #[cfg(target_arch = "wasm32")]
    {
        let window = web_sys::window()?;
        let storage = window.local_storage().ok().flatten()?;
        let v = storage.get_item(&key).ok().flatten()?;
        if v.trim().is_empty() {
            None
        } else {
            Some(v)
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = key;
        None
    }
}

fn write_stored_pref(kind: &str, workspace_id: &str, value: &str) {
    if value.trim().is_empty() {
        return;
    }
    let key = storage_pref_key(kind, workspace_id);
    #[cfg(target_arch = "wasm32")]
    {
        let Some(window) = web_sys::window() else {
            return;
        };
        let Ok(Some(storage)) = window.local_storage() else {
            return;
        };
        let _ = storage.set_item(&key, value);
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = key;
    }
}

fn read_stored_runner(workspace_id: &str) -> Option<String> {
    read_stored_pref("runner", workspace_id)
}

fn write_stored_runner(workspace_id: &str, runner_id: &str) {
    write_stored_pref("runner", workspace_id, runner_id);
}

fn read_stored_effort(workspace_id: &str) -> Option<String> {
    read_stored_pref("effort", workspace_id)
}

fn write_stored_effort(workspace_id: &str, effort: &str) {
    write_stored_pref("effort", workspace_id, effort);
}

fn read_stored_provider(workspace_id: &str) -> Option<String> {
    read_stored_pref("provider", workspace_id)
}

fn write_stored_provider(workspace_id: &str, provider: &str) {
    write_stored_pref("provider", workspace_id, provider);
}

fn read_stored_model(workspace_id: &str) -> Option<String> {
    read_stored_pref("model", workspace_id)
}

fn write_stored_model(workspace_id: &str, model: &str) {
    write_stored_pref("model", workspace_id, model);
}

fn write_stored_composer(workspace_id: &str, provider: &str, model: &str) {
    write_stored_provider(workspace_id, provider);
    write_stored_model(workspace_id, model);
}

const CATALOG_PROVIDER_IDS: &[&str] = &["grok", "claude", "openai", "ollama"];

fn plane_openai_from_status(status: Option<&ChatStatus>) -> Option<&ChatProviderInfo> {
    status.and_then(|s| s.providers.iter().find(|p| p.id == "openai"))
}

fn unavailable_openai() -> ChatProviderInfo {
    ChatProviderInfo {
        id: "openai".into(),
        name: catalog_provider_display_name("openai").to_string(),
        available: false,
        coming_soon: false,
        error: Some("Set OPENAI_API_KEY to enable OpenAI chat.".into()),
        models: vec![],
        default_model: None,
    }
}

fn providers_from_runner(
    host: &HostPublic,
    plane_openai: Option<&ChatProviderInfo>,
) -> Vec<ChatProviderInfo> {
    CATALOG_PROVIDER_IDS
        .iter()
        .map(|id| {
            if *id == "openai" {
                return plane_openai.cloned().unwrap_or_else(unavailable_openai);
            }
            let reported = host.agents.iter().find(|a| a.id == *id);
            let installed = reported.map(|a| a.installed).unwrap_or(false);
            let logged_in = reported.map(|a| a.logged_in).unwrap_or(false);
            let mut models: Vec<ChatModelInfo> = reported
                .map(|a| {
                    a.models
                        .iter()
                        .map(|n| ChatModelInfo {
                            name: n.clone(),
                            size: None,
                            parameter_size: None,
                            family: None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            let phrase = agent_status_phrase(id, installed, logged_in);
            let available = if *id == "ollama" {
                installed && !models.is_empty()
            } else {
                installed && logged_in
            };
            // Claude Code has no `models` command (it hangs). Show official aliases.
            if *id == "claude" && available && models.is_empty() {
                models = claude_cli_model_aliases()
                    .iter()
                    .map(|n| ChatModelInfo {
                        name: (*n).into(),
                        size: None,
                        parameter_size: None,
                        family: None,
                    })
                    .collect();
            }
            let error = if available && (*id == "ollama" || !models.is_empty()) {
                None
            } else {
                Some(phrase.to_string())
            };
            let default_model = reported
                .and_then(|a| a.models.first().cloned())
                .or_else(|| models.first().map(|m| m.name.clone()));
            ChatProviderInfo {
                id: (*id).into(),
                name: catalog_provider_display_name(id).to_string(),
                available,
                coming_soon: false,
                error,
                models,
                default_model,
            }
        })
        .collect()
}

fn provider_candidates(providers: &[ChatProviderInfo]) -> Vec<ChatProviderCandidate> {
    providers
        .iter()
        .map(|p| ChatProviderCandidate {
            id: p.id.clone(),
            available: p.available,
            models: p.models.iter().map(|m| m.name.clone()).collect(),
            default_model: p.default_model.clone(),
        })
        .collect()
}

fn apply_composer_defaults(
    workspace_id: &str,
    host: Option<&HostPublic>,
    plane_openai: Option<&ChatProviderInfo>,
    mut selected_provider: Signal<String>,
    mut selected_model: Signal<String>,
    selected_effort: Signal<String>,
    user_picked: bool,
) {
    let providers = match host {
        Some(h) => providers_from_runner(h, plane_openai),
        None => plane_openai.cloned().into_iter().collect(),
    };
    if providers.is_empty() {
        return;
    }
    let candidates = provider_candidates(&providers);
    if should_apply_chat_defaults(&candidates, &selected_provider(), user_picked) {
        if let Some(ChatProviderPick { provider_id, model }) = pick_chat_provider_and_model(
            &candidates,
            read_stored_provider(workspace_id).as_deref(),
            read_stored_model(workspace_id).as_deref(),
        ) {
            selected_provider.set(provider_id);
            selected_model.set(model);
        }
    }
    if let Some(host) = host {
        apply_effort_for_model(
            host,
            &selected_provider(),
            &selected_model(),
            workspace_id,
            selected_effort,
        );
    }
}

fn apply_effort_for_model(
    host: &HostPublic,
    provider: &str,
    model: &str,
    workspace_id: &str,
    mut selected_effort: Signal<String>,
) {
    let detail = host
        .agents
        .iter()
        .find(|a| a.id == provider)
        .and_then(|a| find_model_detail(&a.model_details, model));
    let Some(value) = pick_effort_value(detail, read_stored_effort(workspace_id).as_deref())
    else {
        return;
    };
    let cur = selected_effort();
    if cur != value {
        selected_effort.set(value);
    }
}

fn provider_option_label(p: &ChatProviderInfo) -> String {
    let phrase = p.error.as_deref().unwrap_or("");
    if phrase.is_empty() {
        p.name.clone()
    } else {
        format!("{} · {phrase}", p.name)
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
    /// Send → reply, milliseconds. Assistant turns only.
    latency_ms: Option<u64>,
}

fn now_ms() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        js_sys::Date::now() as u64
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
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
    runner_id: String,
    effort: String,
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
    if model.trim().is_empty() && provider == "ollama" {
        show_toast().error("Select a model first (`ollama pull …` on the runner).");
        return;
    }
    write_stored_runner(&workspace_id, &runner_id);
    write_stored_effort(&workspace_id, &effort);
    write_stored_composer(&workspace_id, &provider, &model);

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
        latency_ms: None,
    });
    messages.set(list);
    draft.set(String::new());
    sending.set(true);
    let sent_at = now_ms();

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
            latency_ms: None,
        });
        messages.set(list);
    }

    spawn(async move {
        let mut streamed = String::new();
        let stamp = |last: &mut ChatMessage| {
            last.latency_ms = Some(now_ms().saturating_sub(sent_at));
        };
        let result = stream_chat(
            &workspace_id,
            history,
            provider,
            Some(model_for_api.clone()),
            Some(conversation_id),
            if runner_id.trim().is_empty() {
                None
            } else {
                Some(runner_id.clone())
            },
            Some(effort.clone()),
            |ev| match ev {
                ChatStreamEvent::Status { text } => {
                    let mut list = messages();
                    if let Some(last) = list.iter_mut().rev().find(|m| m.id == reply_id) {
                        if last.content.is_empty() || last.content.starts_with('_') {
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
                        stamp(last);
                    }
                    messages.set(list);
                }
                ChatStreamEvent::Error { error } => {
                    let mut list = messages();
                    if let Some(last) = list.iter_mut().rev().find(|m| m.id == reply_id) {
                        last.content = format!("⚠️ {error}");
                        stamp(last);
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
                last.latency_ms = Some(now_ms().saturating_sub(sent_at));
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
    runner_id: String,
    effort: String,
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
    if model.trim().is_empty() && provider == "ollama" {
        show_toast().error("Select a model first (`ollama pull …` on the runner).");
        return;
    }

    if let Some(cid) = conversation_id() {
        send_on_existing_session(
            text,
            workspace_id,
            provider,
            model,
            runner_id,
            effort,
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
    let runner_id = runner_id.clone();

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
                write_stored_runner(&workspace_id, &runner_id);
                write_stored_effort(&workspace_id, &effort);
                write_stored_composer(&workspace_id, &provider_for_create, &model_for_create);
                // Hand off first user message to the new `/chat/{id}` page.
                stash_pending_first_send(PendingFirstSend {
                    conversation_id: conv.id.clone(),
                    text: text.clone(),
                    provider: provider_for_create,
                    model: model_for_create,
                    runner_id,
                    effort,
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
    _selected_provider: Signal<String>,
    _selected_model: Signal<String>,
) {
    // Runner inventory owns provider/model. Status is only the hint / error banner.
    let _ = (_selected_provider, _selected_model);
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
    let mut selected_provider =
        use_signal(|| read_stored_provider(&workspace_id).unwrap_or_default());
    let mut selected_model = use_signal(|| read_stored_model(&workspace_id).unwrap_or_default());
    let mut user_picked_provider = use_signal(|| false);
    let mut selected_runner = use_signal(|| {
        peek_pending_runner(initial_conversation_id.as_deref())
            .or_else(|| read_stored_runner(&workspace_id))
            .unwrap_or_default()
    });
    let mut selected_effort =
        use_signal(|| read_stored_effort(&workspace_id).unwrap_or_default());
    let mut hosts = use_signal(Vec::<HostPublic>::new);
    let status_loading = use_signal(|| true);
    let mut conversation_id = use_signal(|| initial_conversation_id.clone());
    let mut loading_thread = use_signal(|| initial_conversation_id.is_some());
    let mut thread_title = use_signal(|| "New chat".to_string());
    let mut wait_tick = use_signal(|| 0u32);
    let mut send_started_ms = use_signal(|| None::<u64>);
    let navigator = use_navigator();

    use_effect(move || {
        if sending() {
            send_started_ms.set(Some(now_ms()));
            wait_tick.set(0);
            spawn(async move {
                while sending() {
                    TimeoutFuture::new(250).await;
                    wait_tick.set(wait_tick() + 1);
                }
            });
        } else {
            send_started_ms.set(None);
        }
    });

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

    let ws_hosts = workspace_id.clone();
    use_effect(move || {
        let ws = ws_hosts.clone();
        spawn(async move {
            loop {
                if let Ok((list, _, _)) = list_workers_page(&ws).await {
                    let active: Vec<_> = list
                        .into_iter()
                        .filter(|h| !is_removed(h) && h.kind != "control_plane")
                        .collect();
                    if selected_runner().is_empty() {
                        if let Some(h) = active
                            .iter()
                            .find(|h| h.is_this_machine)
                            .or_else(|| active.iter().find(|h| h.is_control_plane_box))
                            .or_else(|| active.iter().find(|h| h.status == "online"))
                        {
                            selected_runner.set(h.id.clone());
                            write_stored_runner(&ws, &h.id);
                        }
                    }
                    let runner = selected_runner();
                    apply_composer_defaults(
                        &ws,
                        active.iter().find(|h| h.id == runner),
                        plane_openai_from_status(status().as_ref()),
                        selected_provider,
                        selected_model,
                        selected_effort,
                        user_picked_provider(),
                    );
                    hosts.set(active);
                }
                TimeoutFuture::new(10_000).await;
            }
        });
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
            selected_runner.set(pending.runner_id.clone());
            selected_effort.set(pending.effort.clone());
            user_picked_provider.set(true);
            write_stored_runner(&ws_load, &pending.runner_id);
            write_stored_effort(&ws_load, &pending.effort);
            write_stored_composer(&ws_load, &pending.provider, &pending.model);
            send_on_existing_session(
                pending.text,
                ws_load.clone(),
                pending.provider,
                pending.model,
                pending.runner_id,
                pending.effort,
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
                        user_picked_provider.set(true);
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
                            latency_ms: None,
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

    let runner_host = hosts().into_iter().find(|h| h.id == selected_runner());
    let status_now = status();
    let plane_openai = plane_openai_from_status(status_now.as_ref());
    let providers: Vec<ChatProviderInfo> = match runner_host.as_ref() {
        Some(h) => providers_from_runner(h, plane_openai),
        None => plane_openai.cloned().into_iter().collect(),
    };

    let active_provider = providers
        .iter()
        .find(|p| p.id == selected_provider())
        .cloned();

    let models: Vec<ChatModelInfo> = active_provider
        .as_ref()
        .map(|p| p.models.clone())
        .unwrap_or_default();

    let model_detail = runner_host.as_ref().and_then(|h| {
        h.agents
            .iter()
            .find(|a| a.id == selected_provider())
            .and_then(|a| find_model_detail(&a.model_details, &selected_model()))
    });
    let effort_choices: Vec<(String, String)> = model_detail
        .map(|d| {
            d.efforts
                .iter()
                .map(|e| {
                    (
                        e.value.clone(),
                        e.label.clone().unwrap_or_else(|| e.value.clone()),
                    )
                })
                .collect()
        })
        .unwrap_or_else(|| {
            if selected_provider() == "claude"
                && active_provider.as_ref().is_some_and(|p| p.available)
            {
                claude_effort_values()
                    .iter()
                    .map(|v| ((*v).to_string(), (*v).to_string()))
                    .collect()
            } else {
                Vec::new()
            }
        });
    let preferred_effort = {
        let cur = selected_effort();
        if !cur.is_empty() {
            Some(cur)
        } else {
            read_stored_effort(&workspace_id)
        }
    };
    let resolved_effort = pick_effort_value(model_detail, preferred_effort.as_deref())
        .unwrap_or_else(|| selected_effort());

    let openai_plane_available = providers
        .iter()
        .find(|p| p.id == "openai")
        .is_some_and(|p| p.available);
    let can_chat = if selected_provider() == "openai" && openai_plane_available {
        true
    } else {
        !selected_runner().is_empty() && active_provider.as_ref().is_some_and(|p| p.available)
    };
    let hosts_ready = !hosts().is_empty() || !selected_runner().is_empty();
    let empty_hint = if selected_runner().is_empty() && selected_provider() != "openai" {
        "Select a runner. Login and models are whatever that box last reported.".to_string()
    } else if let Some(p) = active_provider.as_ref() {
        format!(
            "{} · {} on this runner.",
            p.name,
            p.error
                .clone()
                .unwrap_or_else(|| agent_status_phrase(&p.id, true, true).to_string())
        )
    } else {
        "Pick a provider on the selected runner.".into()
    };

    let thinking_label = {
        let _ = wait_tick();
        let clock = send_started_ms()
            .map(|t| now_ms().saturating_sub(t))
            .filter(|ms| *ms > 0)
            .map(|ms| format!(" · {}", format_chat_elapsed(ms)))
            .unwrap_or_default();
        let m = selected_model();
        if m.is_empty() {
            format!("Thinking…{clock}")
        } else {
            format!("Thinking with {m}…{clock}")
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
                            } else {
                                "{empty_hint}"
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
                        if let Some(err) = active_provider.as_ref().and_then(|p| p.error.clone()) {
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
                                "Grok login or Private (Ollama) required…"
                            },
                            value: "{draft}",
                            disabled: !can_chat || sending(),
                            oninput: move |e| draft.set(e.value()),
                            onkeydown: {
                                let ws = workspace_id.clone();
                                let effort = resolved_effort.clone();
                                move |e| {
                                    if e.key() == Key::Enter && !e.modifiers().shift() {
                                        e.prevent_default();
                                        if !can_chat {
                                            show_toast().error(
                                                status()
                                                    .and_then(|s| s.error.clone())
                                                    .unwrap_or_else(|| {
                                                        "No chat provider ready (Grok or Private (Ollama))"
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
                                            selected_runner(),
                                            effort.clone(),
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
                                    "Runner"
                                }
                                select {
                                    class: "max-w-[9rem] cursor-pointer truncate rounded-lg border-0 bg-[var(--app-bg)] px-2 py-1.5 text-xs font-medium text-[var(--app-fg)] outline-none",
                                    value: "{selected_runner}",
                                    title: "Self-hosted runner for this chat",
                                    onchange: {
                                        let ws = workspace_id.clone();
                                        move |e| {
                                            let id = e.value();
                                            if id.is_empty() {
                                                return;
                                            }
                                            selected_runner.set(id.clone());
                                            write_stored_runner(&ws, &id);
                                            user_picked_provider.set(false);
                                            apply_composer_defaults(
                                                &ws,
                                                hosts().iter().find(|h| h.id == id),
                                                plane_openai_from_status(status().as_ref()),
                                                selected_provider,
                                                selected_model,
                                                selected_effort,
                                                false,
                                            );
                                        }
                                    },
                                    option { value: "", "Select runner" }
                                    if !selected_runner().is_empty()
                                        && !hosts().iter().any(|h| h.id == selected_runner())
                                    {
                                        option {
                                            value: "{selected_runner}",
                                            selected: true,
                                            "Selected runner"
                                        }
                                    }
                                    for h in hosts() {
                                        option {
                                            key: "{h.id}",
                                            value: "{h.id}",
                                            selected: selected_runner() == h.id,
                                            "{display_name(&h)}"
                                        }
                                    }
                                }
                            }

                            div { class: "flex min-w-0 items-center gap-1.5",
                                span {
                                    class: "hidden text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)] sm:inline",
                                    title: "Fleet agents use this same provider and model",
                                    "Provider"
                                }
                                select {
                                    class: "max-w-[10rem] cursor-pointer truncate rounded-lg border-0 bg-[var(--app-bg)] px-2 py-1.5 text-xs font-medium text-[var(--app-fg)] outline-none ring-0 focus:border-0 focus:outline-none focus:ring-0 disabled:opacity-50",
                                    value: "{selected_provider}",
                                    disabled: sending() || providers.is_empty(),
                                    title: "Chat provider",
                                    onchange: {
                                        let providers = providers.clone();
                                        let ws = workspace_id.clone();
                                        move |e| {
                                            let id = e.value();
                                            selected_provider.set(id.clone());
                                            user_picked_provider.set(true);
                                            let model = if let Some(p) =
                                                providers.iter().find(|p| p.id == id)
                                            {
                                                if !p.available {
                                                    show_toast().error(
                                                        p.error.clone().unwrap_or_else(|| {
                                                            format!("{} is not ready on that runner", p.name)
                                                        }),
                                                    );
                                                }
                                                pick_default_model(p)
                                            } else {
                                                String::new()
                                            };
                                            selected_model.set(model.clone());
                                            write_stored_composer(&ws, &id, &model);
                                            if let Some(h) = hosts().iter().find(|h| h.id == selected_runner())
                                            {
                                                apply_effort_for_model(
                                                    h,
                                                    &id,
                                                    &model,
                                                    &ws,
                                                    selected_effort,
                                                );
                                            }
                                        }
                                    },
                                    if providers.is_empty() {
                                        option { value: "", "Pick a runner first" }
                                    } else {
                                        for p in providers.iter() {
                                            option {
                                                key: "{p.id}",
                                                value: "{p.id}",
                                                selected: selected_provider() == p.id,
                                                disabled: p.coming_soon,
                                                { provider_option_label(p) }
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
                                    disabled: sending()
                                        || providers.is_empty()
                                        || (models.is_empty() && selected_provider() == "ollama"),
                                    title: "Model for the selected provider",
                                    onchange: {
                                        let details = runner_host
                                            .as_ref()
                                            .and_then(|h| {
                                                h.agents.iter().find(|a| a.id == selected_provider())
                                            })
                                            .map(|a| a.model_details.clone())
                                            .unwrap_or_default();
                                        let ws = workspace_id.clone();
                                        move |e| {
                                            let name = e.value();
                                            selected_model.set(name.clone());
                                            write_stored_composer(&ws, &selected_provider(), &name);
                                            if let Some(value) = pick_effort_value(
                                                find_model_detail(&details, &name),
                                                read_stored_effort(&ws).as_deref(),
                                            ) {
                                                selected_effort.set(value);
                                            }
                                        }
                                    },
                                    if models.is_empty() {
                                        option {
                                            value: "",
                                            if !hosts_ready && selected_provider() != "openai" {
                                                "Loading runner inventory…"
                                            } else if selected_runner().is_empty()
                                                && selected_provider() != "openai"
                                            {
                                                "Pick a runner first"
                                            } else if selected_provider() != "ollama"
                                                && active_provider.as_ref().is_some_and(|p| p.available)
                                            {
                                                "CLI default"
                                            } else if let Some(p) = active_provider.as_ref() {
                                                { p.error.clone().unwrap_or_else(|| "No models reported".into()) }
                                            } else {
                                                "No models reported"
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

                            if !effort_choices.is_empty() {
                            div { class: "flex min-w-0 items-center gap-1.5",
                                span {
                                    class: "hidden text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)] sm:inline",
                                    "Effort"
                                }
                                select {
                                    class: "max-w-[8rem] cursor-pointer truncate rounded-lg border-0 bg-[var(--app-bg)] px-2 py-1.5 text-xs font-medium text-[var(--app-fg)] outline-none",
                                    value: "{resolved_effort}",
                                    title: "Reasoning effort advertised by this model",
                                    onchange: {
                                        let ws = workspace_id.clone();
                                        move |e| {
                                            let v = e.value();
                                            selected_effort.set(v.clone());
                                            write_stored_effort(&ws, &v);
                                        }
                                    },
                                    for (value, label) in effort_choices.iter() {
                                        option {
                                            value: "{value}",
                                            selected: resolved_effort == *value,
                                            "{label}"
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
                                disabled: sending(),
                                onclick: {
                                    let ws = workspace_id.clone();
                                    move |_| {
                                        let ws = ws.clone();
                                        spawn(async move {
                                            if let Ok((list, _, _)) = list_workers_page(&ws).await {
                                                let active: Vec<_> = list
                                                    .into_iter()
                                                    .filter(|h| !is_removed(h) && h.kind != "control_plane")
                                                    .collect();
                                                let runner = selected_runner();
                                                apply_composer_defaults(
                                                    &ws,
                                                    active.iter().find(|h| h.id == runner),
                                                    plane_openai_from_status(status().as_ref()),
                                                    selected_provider,
                                                    selected_model,
                                                    selected_effort,
                                                    user_picked_provider(),
                                                );
                                                hosts.set(active);
                                            }
                                        });
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
                                    let effort = resolved_effort.clone();
                                    move |_| {
                                        send_message(
                                            draft(),
                                            ws.clone(),
                                            selected_provider(),
                                            selected_model(),
                                            selected_runner(),
                                            effort.clone(),
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
                p { class: "pointer-events-auto mx-auto mt-1.5 max-w-3xl px-1 text-[10px] leading-4 text-[var(--app-muted)]",
                    "Grok, Claude, and Private (Ollama) turns run on the selected runner’s official CLI. OpenAI still uses the API key on the control plane."
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
    let shown = if !is_user
        && looks_like_later_copy(&message.content)
        && message
            .actions
            .iter()
            .any(|a| a.ok && is_fleet_launch(&a.tool))
    {
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
                if message.model.is_some() || message.latency_ms.is_some() {
                    p { class: "m-0 mt-3 text-[11px] text-[var(--app-muted)]",
                        {
                            match (&message.model, message.latency_ms) {
                                (Some(model), Some(ms)) => {
                                    format!("{model} · {}", format_chat_elapsed(ms))
                                }
                                (Some(model), None) => model.clone(),
                                (None, Some(ms)) => format_chat_elapsed(ms),
                                (None, None) => String::new(),
                            }
                        }
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
        Some(format!(
            "{}…",
            chars.into_iter().take(137).collect::<String>()
        ))
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
                                    .map(|id| {
                                        id == rid || id.starts_with(&rid) || rid.starts_with(id)
                                    })
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
                                    Some((a.get("profile")?.as_str()?.to_string(), kind, text))
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
                                        m.get("to")
                                            .and_then(|x| x.as_str())
                                            .unwrap_or("*")
                                            .to_string(),
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
