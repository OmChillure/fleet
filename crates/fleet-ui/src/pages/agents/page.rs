use dioxus::prelude::*;

use super::api::{
    PersonaDto, WorkspacePersona, archive_workspace_persona, create_workspace_persona,
    fetch_personas, list_workspace_personas, seed_starters,
};
use crate::components::toast::show_toast;

#[component]
pub fn Agents(workspace_id: String) -> Element {
    let mut loading = use_signal(|| true);
    let mut error = use_signal(|| None::<String>);
    let mut personas = use_signal(Vec::<PersonaDto>::new);
    let mut workspace_personas = use_signal(Vec::<WorkspacePersona>::new);
    let mut backend = use_signal(|| None::<String>);
    let mut source = use_signal(String::new);
    let mut seeding = use_signal(|| false);
    let mut creating = use_signal(|| false);
    let mut new_name = use_signal(String::new);
    let mut new_desc = use_signal(String::new);
    let mut new_prompt = use_signal(String::new);
    let mut new_profile = use_signal(|| "coding".to_string());
    let ws = workspace_id.clone();

    use_effect(move || {
        let ws = ws.clone();
        spawn(async move {
            match fetch_personas(&ws).await {
                Ok(p) => {
                    source.set(p.source);
                    backend.set(p.agent_backend);
                    personas.set(p.personas);
                    error.set(None);
                }
                Err(e) => error.set(Some(e)),
            }
            if let Ok(custom) = list_workspace_personas(&ws).await {
                workspace_personas.set(custom);
            }
            loading.set(false);
        });
    });

    rsx! {
        div { class: "mx-auto max-w-5xl px-4 py-6 md:px-6",
            h1 { class: "m-0 text-2xl font-semibold tracking-tight text-[var(--app-fg)]",
                "Agents"
            }
            p { class: "mt-1 text-sm text-[var(--app-muted)]",
                "Personas configure future runs. No LLM is called here. "
                "Roster slugs go on launch_agent_plan. Workspace personas are saved here (seed starters or create your own). "
                if let Some(b) = backend() {
                    "LLM backend: {b}."
                } else {
                    "Pick Fleet LLM in Settings (Ollama), or set FLEET_AGENT_BACKEND=grok on the host."
                }
            }
            if !source().is_empty() {
                p { class: "mt-1 text-xs text-[var(--app-muted)]",
                    "Roster: {source}"
                }
            }

            if loading() {
                p { class: "mt-8 text-sm text-[var(--app-muted)]", "Loading personas…" }
            } else if let Some(err) = error() {
                p { class: "mt-8 text-sm text-[var(--app-muted)]", "{err}" }
            } else {
                div { class: "mt-6 grid gap-3 sm:grid-cols-2 lg:grid-cols-3",
                    for p in personas() {
                        article {
                            key: "{p.slug}",
                            class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                            p { class: "m-0 text-[10px] uppercase tracking-wide text-[var(--app-muted)]",
                                "{p.division}"
                            }
                            h2 { class: "m-0 mt-1 text-sm font-semibold text-[var(--app-fg)]",
                                "{p.emoji} {p.name}"
                            }
                            p { class: "m-0 mt-1 font-mono text-[11px] text-[var(--app-muted)]",
                                "{p.slug}"
                            }
                            if !p.vibe.is_empty() {
                                p { class: "m-0 mt-2 text-xs leading-5 text-[var(--app-fg)]",
                                    "{p.vibe}"
                                }
                            } else if !p.description.is_empty() {
                                p { class: "m-0 mt-2 text-xs leading-5 text-[var(--app-muted)]",
                                    "{p.description}"
                                }
                            }
                        }
                    }
                }
            }

            section { class: "mt-10",
                div { class: "flex flex-wrap items-center justify-between gap-2",
                    h2 { class: "m-0 text-lg font-semibold text-[var(--app-fg)]",
                        "Workspace personas"
                    }
                    button {
                        class: "rounded-lg border border-[var(--app-border)] px-3 py-1.5 text-sm text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)] disabled:opacity-50",
                        r#type: "button",
                        disabled: seeding(),
                        onclick: {
                            let ws = workspace_id.clone();
                            move |_| {
                                if seeding() { return; }
                                seeding.set(true);
                                let ws = ws.clone();
                                spawn(async move {
                                    match seed_starters(&ws).await {
                                        Ok(r) => {
                                            show_toast().success(format!(
                                                "Seeded {0} starter(s), {1} already present.",
                                                r.inserted, r.skipped
                                            ));
                                            if let Ok(list) = list_workspace_personas(&ws).await {
                                                workspace_personas.set(list);
                                            }
                                        }
                                        Err(e) => show_toast().error(e),
                                    }
                                    seeding.set(false);
                                });
                            }
                        },
                        if seeding() { "Seeding…" } else { "Seed starters" }
                    }
                }
                p { class: "mt-1 text-xs text-[var(--app-muted)]",
                    "Stored on this workspace. Seed adds the 8 starter roles if missing."
                }

                div { class: "mt-4 grid gap-3 sm:grid-cols-2",
                    input {
                        class: "rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                        r#type: "text",
                        placeholder: "Name",
                        value: "{new_name}",
                        oninput: move |e| new_name.set(e.value()),
                    }
                    input {
                        class: "rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                        r#type: "text",
                        placeholder: "Worker profile (coding, reviewer, …)",
                        value: "{new_profile}",
                        oninput: move |e| new_profile.set(e.value()),
                    }
                }
                input {
                    class: "mt-2 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                    r#type: "text",
                    placeholder: "Short description",
                    value: "{new_desc}",
                    oninput: move |e| new_desc.set(e.value()),
                }
                textarea {
                    class: "mt-2 w-full rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 text-sm text-[var(--app-fg)]",
                    rows: "3",
                    placeholder: "Role prompt",
                    value: "{new_prompt}",
                    oninput: move |e| new_prompt.set(e.value()),
                }
                button {
                    class: "mt-2 rounded-lg bg-[var(--app-fg)] px-3 py-2 text-sm font-semibold text-[var(--app-bg)] disabled:opacity-50",
                    r#type: "button",
                    disabled: creating() || new_name().trim().is_empty(),
                    onclick: {
                        let ws = workspace_id.clone();
                        move |_| {
                            if creating() { return; }
                            let name = new_name().trim().to_string();
                            if name.is_empty() { return; }
                            creating.set(true);
                            let ws = ws.clone();
                            let desc = new_desc();
                            let prompt = new_prompt();
                            let profile = new_profile();
                            spawn(async move {
                                match create_workspace_persona(&ws, &name, &desc, &prompt, &profile).await {
                                    Ok(_) => {
                                        show_toast().success("Persona created.");
                                        new_name.set(String::new());
                                        new_desc.set(String::new());
                                        new_prompt.set(String::new());
                                        if let Ok(list) = list_workspace_personas(&ws).await {
                                            workspace_personas.set(list);
                                        }
                                    }
                                    Err(e) => show_toast().error(e),
                                }
                                creating.set(false);
                            });
                        }
                    },
                    if creating() { "Creating…" } else { "Create persona" }
                }

                if workspace_personas().is_empty() {
                    p { class: "mt-4 text-sm text-[var(--app-muted)]",
                        "None yet. Seed starters or create one."
                    }
                } else {
                    ul { class: "mt-4 m-0 list-none space-y-2 p-0",
                        for p in workspace_personas() {
                            li {
                                key: "{p.id}",
                                class: "flex flex-wrap items-start justify-between gap-2 rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] px-4 py-3",
                                div { class: "min-w-0",
                                    p { class: "m-0 text-sm font-medium text-[var(--app-fg)]",
                                        "{p.name}"
                                        if p.is_starter {
                                            span { class: "ml-2 text-[10px] uppercase text-[var(--app-muted)]", "starter" }
                                        }
                                    }
                                    p { class: "m-0 mt-0.5 font-mono text-[11px] text-[var(--app-muted)]",
                                        "{p.slug} · {p.worker_profile} · {p.status}"
                                    }
                                    if !p.description.is_empty() {
                                        p { class: "m-0 mt-1 text-xs text-[var(--app-muted)]",
                                            "{p.description}"
                                        }
                                    }
                                }
                                if p.status != "archived" {
                                    button {
                                        class: "rounded-md border border-red-500/40 px-2 py-1 text-[11px] text-red-400 hover:bg-red-500/10",
                                        r#type: "button",
                                        onclick: {
                                            let ws = workspace_id.clone();
                                            let id = p.id.clone();
                                            move |_| {
                                                let ws = ws.clone();
                                                let id = id.clone();
                                                spawn(async move {
                                                    match archive_workspace_persona(&ws, &id).await {
                                                        Ok(()) => {
                                                            show_toast().success("Archived.");
                                                            if let Ok(list) = list_workspace_personas(&ws).await {
                                                                workspace_personas.set(list);
                                                            }
                                                        }
                                                        Err(e) => show_toast().error(e),
                                                    }
                                                });
                                            }
                                        },
                                        "Archive"
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
