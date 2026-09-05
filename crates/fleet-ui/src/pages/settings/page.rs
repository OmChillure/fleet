//! Workspace Settings: concurrent agents + local fleet LLM (Ollama). Grok is env-only.

use dioxus::prelude::*;

use super::api::{
    FleetSettingsPatch, InviteRow, MemberRow, fetch_settings, invite_member, list_invites,
    list_members, remove_member, revoke_invite, save_settings, set_member_role,
};
use crate::components::toast::show_toast;
use crate::pages::chat::{ChatModelInfo, fetch_chat_status};

fn field_class() -> &'static str {
    "mt-1 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)] outline-none focus:border-[var(--app-muted)]"
}

#[component]
pub fn Settings(workspace_id: String) -> Element {
    let mut loading = use_signal(|| true);
    let mut saving = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut max_agents = use_signal(|| 16i32);
    let mut max_cap = use_signal(|| 256i32);
    let mut fleet_llm = use_signal(|| "off".to_string());
    let mut fleet_model = use_signal(String::new);
    let mut grok_env = use_signal(|| false);
    let mut min_isolation = use_signal(|| "process".to_string());
    let mut effective = use_signal(|| None::<String>);
    let mut ollama_models = use_signal(Vec::<ChatModelInfo>::new);
    let mut ollama_error = use_signal(|| None::<String>);
    let mut members = use_signal(Vec::<MemberRow>::new);
    let mut invites = use_signal(Vec::<InviteRow>::new);
    let mut my_role = use_signal(|| "member".to_string());
    let mut my_user_id = use_signal(String::new);
    let mut invite_email = use_signal(String::new);
    let mut invite_role = use_signal(|| "member".to_string());

    let ws_load = workspace_id.clone();
    use_effect(move || {
        let ws = ws_load.clone();
        spawn(async move {
            match fetch_settings(&ws).await {
                Ok(s) => {
                    max_agents.set(s.max_agents);
                    max_cap.set(s.max_agents_cap.max(1));
                    fleet_llm.set(s.fleet_llm);
                    fleet_model.set(s.fleet_llm_model);
                    grok_env.set(s.grok_env);
                    if !s.min_isolation.trim().is_empty() {
                        min_isolation.set(s.min_isolation);
                    }
                    effective.set(s.effective_backend);
                    error.set(None);
                }
                Err(e) => error.set(Some(e)),
            }
            match fetch_chat_status(&ws).await {
                Ok(st) => {
                    let ollama = st.providers.iter().find(|p| p.id == "ollama");
                    if let Some(p) = ollama {
                        ollama_models.set(p.models.clone());
                        ollama_error.set(p.error.clone());
                        if fleet_model().is_empty() {
                            if let Some(d) = &p.default_model {
                                fleet_model.set(d.clone());
                            } else if let Some(first) = p.models.first() {
                                fleet_model.set(first.name.clone());
                            }
                        }
                    }
                }
                Err(e) => ollama_error.set(Some(e.message())),
            }
            if let Ok(m) = list_members(&ws).await {
                my_role.set(m.you.role);
                my_user_id.set(m.you.user_id);
                members.set(m.members);
            }
            if let Ok(inv) = list_invites(&ws).await {
                invites.set(inv);
            }
            loading.set(false);
        });
    });

    let ws_save = workspace_id.clone();
    let on_save = move |_| {
        if saving() {
            return;
        }
        saving.set(true);
        let ws = ws_save.clone();
        let n = max_agents();
        let llm = fleet_llm();
        let model = fleet_model();
        let iso = min_isolation();
        spawn(async move {
            let s = match save_settings(
                &ws,
                &FleetSettingsPatch {
                    max_agents: n,
                    fleet_llm: llm,
                    fleet_llm_model: model,
                    min_isolation: iso,
                },
            )
            .await
            {
                Ok(s) => s,
                Err(e) => {
                    show_toast().error(e.clone());
                    error.set(Some(e));
                    saving.set(false);
                    return;
                }
            };
            max_agents.set(s.max_agents);
            fleet_llm.set(s.fleet_llm);
            fleet_model.set(s.fleet_llm_model);
            if !s.min_isolation.trim().is_empty() {
                min_isolation.set(s.min_isolation);
            }
            grok_env.set(s.grok_env);
            effective.set(s.effective_backend);
            show_toast().success("Settings saved.");
            error.set(None);
            saving.set(false);
        });
    };

    let models = ollama_models();
    let llm_off = fleet_llm() == "off";

    rsx! {
        div { class: "mx-auto max-w-xl px-4 py-6 md:px-6",
            h1 { class: "m-0 text-2xl font-semibold tracking-tight text-[var(--app-fg)]",
                "Settings"
            }
            p { class: "mt-1 text-sm text-[var(--app-muted)]",
                "How many agents run at once. Workers launched from Chat use the model in the composer dropdown, not this page. Settings LLM is only a fallback if you never chatted. Connect apps under Integrations if the fleet should act there."
            }

            if loading() {
                p { class: "mt-8 text-sm text-[var(--app-muted)]", "Loading…" }
            } else {
                if let Some(err) = error() {
                    p { class: "mt-4 text-sm text-red-400", "{err}" }
                }

                if grok_env() {
                    p { class: "mt-4 rounded-xl border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs leading-5 text-[var(--app-fg)]",
                        "This API process has FLEET_AGENT_BACKEND=grok as a fallback label. Fleet workers still run on a paired runner — pick Grok / Claude / Ollama in Chat and select that box."
                    }
                } else if let Some(b) = effective().filter(|b| b != "off") {
                    p { class: "mt-4 text-xs text-[var(--app-muted)]",
                        "Workers currently use {b}."
                    }
                } else {
                    p { class: "mt-4 text-xs text-[var(--app-muted)]",
                        "Workers use the Chat composer model on the selected runner. This page is only a fallback when a run has no chat model."
                    }
                }

                section { class: "mt-6 rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                    h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                        "Isolation"
                    }
                    p { class: "mt-1 text-xs text-[var(--app-muted)]",
                        "Weakest isolation a host must offer (process < container < vm). k8s is placement, not an isolation class. The control plane does not run sandboxes."
                    }
                    select {
                        class: field_class(),
                        value: "{min_isolation}",
                        onchange: move |e| min_isolation.set(e.value()),
                        option { value: "process", "Process" }
                        option { value: "container", "Container" }
                        option { value: "vm", "VM" }
                    }
                }

                section { class: "mt-4 rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                    h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                        "Number of agents"
                    }
                    p { class: "mt-1 text-xs text-[var(--app-muted)]",
                        "How many sandboxes can run at once. Extra tasks queue. Max {max_cap}."
                    }
                    input {
                        class: field_class(),
                        r#type: "number",
                        min: "1",
                        max: "{max_cap}",
                        value: "{max_agents}",
                        oninput: move |e| {
                            if let Ok(n) = e.value().parse::<i32>() {
                                max_agents.set(n.clamp(1, max_cap()));
                            }
                        },
                    }
                }

                section { class: "mt-4 rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                    h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]",
                        "Fleet LLM"
                    }
                    p { class: "mt-1 text-xs text-[var(--app-muted)]",
                        "Fallback only. Chat-launched specialists use whatever you pick next to the input (Grok, Ollama, OpenAI, Claude)."
                    }
                    label { class: "mt-3 block text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)]",
                        "Backend"
                    }
                    select {
                        class: field_class(),
                        value: "{fleet_llm}",
                        onchange: move |e| {
                            let v = e.value();
                            fleet_llm.set(v.clone());
                            if v == "ollama" && fleet_model().is_empty() {
                                if let Some(m) = ollama_models().first() {
                                    fleet_model.set(m.name.clone());
                                }
                            }
                        },
                        option { value: "off", "Off — bash fences only" }
                        option { value: "ollama", "Ollama (local)" }
                    }

                    if !llm_off {
                        label { class: "mt-3 block text-[10px] font-medium uppercase tracking-wide text-[var(--app-muted)]",
                            "Model"
                        }
                        select {
                            class: field_class(),
                            value: "{fleet_model}",
                            onchange: move |e| fleet_model.set(e.value()),
                            if models.is_empty() {
                                option {
                                    value: "{fleet_model}",
                                    if let Some(err) = ollama_error() {
                                        "{err}"
                                    } else if fleet_model().is_empty() {
                                        "No models — ollama pull …"
                                    } else {
                                        "{fleet_model}"
                                    }
                                }
                            } else {
                                for m in models.iter() {
                                    option {
                                        key: "{m.name}",
                                        value: "{m.name}",
                                        selected: fleet_model() == m.name,
                                        "{m.name}"
                                    }
                                }
                            }
                        }
                        if let Some(err) = ollama_error() {
                            p { class: "mt-2 text-xs text-[var(--app-muted)]", "{err}" }
                        }
                    }
                }

                button {
                    class: "mt-5 rounded-lg border border-[var(--app-border)] bg-[var(--app-bg-elevated)] px-3 py-2 text-sm font-medium text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)] disabled:opacity-50",
                    disabled: saving(),
                    onclick: on_save,
                    if saving() { "Saving…" } else { "Save" }
                }

                section { class: "mt-8 rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                    h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Members" }
                    p { class: "mt-1 text-xs text-[var(--app-muted)]",
                        "Invite by email. Existing users join immediately; others join on signup or next login. Roles: owner, admin, member."
                    }
                    ul { class: "mt-3 m-0 list-none space-y-2 p-0",
                        for m in members() {
                            li {
                                key: "{m.user_id}",
                                class: "flex flex-wrap items-center justify-between gap-2 rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2",
                                div {
                                    p { class: "m-0 text-sm text-[var(--app-fg)]", "{m.email}" }
                                    p { class: "m-0 text-[11px] text-[var(--app-muted)]",
                                        if let Some(n) = &m.display_name {
                                            "{n} · {m.role}"
                                        } else {
                                            "{m.role}"
                                        }
                                    }
                                }
                                div { class: "flex items-center gap-2",
                                    if my_role() == "owner" && m.user_id != my_user_id() {
                                        select {
                                            class: "rounded-md border border-[var(--app-border)] bg-[var(--app-bg)] px-2 py-1 text-xs text-[var(--app-fg)]",
                                            value: "{m.role}",
                                            onchange: {
                                                let ws = workspace_id.clone();
                                                let uid = m.user_id.clone();
                                                move |e| {
                                                    let role = e.value();
                                                    let ws = ws.clone();
                                                    let uid = uid.clone();
                                                    spawn(async move {
                                                        match set_member_role(&ws, &uid, &role).await {
                                                            Ok(_) => {
                                                                if let Ok(list) = list_members(&ws).await {
                                                                    members.set(list.members);
                                                                }
                                                                show_toast().success("Role updated.");
                                                            }
                                                            Err(err) => show_toast().error(err),
                                                        }
                                                    });
                                                }
                                            },
                                            option { value: "owner", "owner" }
                                            option { value: "admin", "admin" }
                                            option { value: "member", "member" }
                                        }
                                    }
                                    if (my_role() == "owner" || my_role() == "admin" || m.user_id == my_user_id())
                                        && !(m.role == "owner" && members().iter().filter(|x| x.role == "owner").count() <= 1)
                                    {
                                        button {
                                            class: "rounded-md border border-red-500/40 px-2 py-1 text-[11px] text-red-400 hover:bg-red-500/10",
                                            r#type: "button",
                                            onclick: {
                                                let ws = workspace_id.clone();
                                                let uid = m.user_id.clone();
                                                move |_| {
                                                    let ws = ws.clone();
                                                    let uid = uid.clone();
                                                    spawn(async move {
                                                        match remove_member(&ws, &uid).await {
                                                            Ok(()) => {
                                                                if let Ok(list) = list_members(&ws).await {
                                                                    members.set(list.members);
                                                                    my_role.set(list.you.role);
                                                                }
                                                                show_toast().success("Member removed.");
                                                            }
                                                            Err(err) => show_toast().error(err),
                                                        }
                                                    });
                                                }
                                            },
                                            if m.user_id == my_user_id() { "Leave" } else { "Remove" }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if my_role() == "owner" || my_role() == "admin" {
                        div { class: "mt-4 flex flex-col gap-2 sm:flex-row",
                            input {
                                class: field_class(),
                                r#type: "email",
                                placeholder: "teammate@example.test",
                                value: "{invite_email}",
                                oninput: move |e| invite_email.set(e.value()),
                            }
                            select {
                                class: field_class(),
                                value: "{invite_role}",
                                onchange: move |e| invite_role.set(e.value()),
                                option { value: "member", "member" }
                                if my_role() == "owner" {
                                    option { value: "admin", "admin" }
                                }
                            }
                            button {
                                class: "rounded-lg bg-[var(--app-fg)] px-3 py-2 text-sm font-semibold text-[var(--app-bg)] disabled:opacity-50",
                                r#type: "button",
                                disabled: invite_email().trim().is_empty(),
                                onclick: {
                                    let workspace_id = workspace_id.clone();
                                    move |_| {
                                        let email = invite_email().trim().to_string();
                                        if email.is_empty() { return; }
                                        let ws = workspace_id.clone();
                                        let role = invite_role();
                                        spawn(async move {
                                            match invite_member(&ws, &email, &role).await {
                                                Ok(v) => {
                                                    invite_email.set(String::new());
                                                    if v.get("added_existing_user").and_then(|x| x.as_bool()) == Some(true) {
                                                        show_toast().success("Added existing user.");
                                                    } else {
                                                        show_toast().success("Invite sent — they join on signup or login.");
                                                    }
                                                    if let Ok(list) = list_members(&ws).await {
                                                        members.set(list.members);
                                                    }
                                                    if let Ok(inv) = list_invites(&ws).await {
                                                        invites.set(inv);
                                                    }
                                                }
                                                Err(err) => show_toast().error(err),
                                            }
                                        });
                                    }
                                },
                                "Invite"
                            }
                        }
                        if !invites().is_empty() {
                            ul { class: "mt-3 m-0 list-none space-y-1 p-0",
                                for inv in invites() {
                                    li {
                                        key: "{inv.id}",
                                        class: "flex items-center justify-between gap-2 text-xs text-[var(--app-muted)]",
                                        span { "{inv.email} · {inv.role} (pending)" }
                                        button {
                                            class: "text-red-400 hover:underline",
                                            r#type: "button",
                                            onclick: {
                                                let ws = workspace_id.clone();
                                                let id = inv.id.clone();
                                                move |_| {
                                                    let ws = ws.clone();
                                                    let id = id.clone();
                                                    spawn(async move {
                                                        match revoke_invite(&ws, &id).await {
                                                            Ok(()) => {
                                                                if let Ok(inv) = list_invites(&ws).await {
                                                                    invites.set(inv);
                                                                }
                                                            }
                                                            Err(err) => show_toast().error(err),
                                                        }
                                                    });
                                                }
                                            },
                                            "Revoke"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
