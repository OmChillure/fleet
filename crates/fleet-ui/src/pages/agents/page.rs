//! Agents & personas — catalog, seed starters, create/edit.

use dioxus::prelude::*;

use super::api::{
    CreatePersonaBody, PatchPersonaBody, Persona, archive_persona, create_persona, csv_to_tools,
    list_personas, seed_starters, tools_to_csv, update_persona,
};
use crate::components::toast::show_toast;

fn icon_label(icon: &str) -> &'static str {
    match icon {
        "code" => "<>",
        "sales" => "$",
        "megaphone" => "📢",
        "support" => "💬",
        "chart" => "📊",
        "ops" => "⚙",
        "compass" => "🧭",
        "docs" => "📄",
        _ => "🤖",
    }
}

#[component]
pub fn Agents(workspace_id: String) -> Element {
    let mut personas = use_signal(|| Vec::<Persona>::new());
    let mut loading = use_signal(|| true);
    let mut busy = use_signal(|| false);
    let mut selected_id = use_signal(|| Option::<String>::None);
    let mut show_create = use_signal(|| false);

    // form fields
    let mut name = use_signal(String::new);
    let mut description = use_signal(String::new);
    let mut role_prompt = use_signal(String::new);
    let mut worker_profile = use_signal(|| "coding".to_string());
    let mut model_preference = use_signal(|| "auto".to_string());
    let mut tools_csv = use_signal(String::new);
    let mut max_budget = use_signal(String::new);
    let mut is_default = use_signal(|| false);

    // initial load
    {
        let ws = workspace_id.clone();
        use_effect(move || {
            let ws = ws.clone();
            spawn(async move {
                loading.set(true);
                match list_personas(&ws).await {
                    Ok(list) => {
                        personas.set(list);
                        loading.set(false);
                    }
                    Err(e) => {
                        loading.set(false);
                        show_toast().error(e.message());
                    }
                }
            });
        });
    }

    let mut open_edit = move |p: Persona| {
        selected_id.set(Some(p.id.clone()));
        show_create.set(false);
        name.set(p.name.clone());
        description.set(p.description.clone());
        role_prompt.set(p.role_prompt.clone());
        worker_profile.set(p.worker_profile.clone());
        model_preference.set(p.model_preference.clone().unwrap_or_else(|| "auto".into()));
        tools_csv.set(tools_to_csv(&p.tools));
        max_budget.set(
            p.max_budget_usd
                .map(|n| format!("{n}"))
                .unwrap_or_default(),
        );
        is_default.set(p.is_default);
    };

    let open_create = move |_| {
        selected_id.set(None);
        show_create.set(true);
        name.set(String::new());
        description.set(String::new());
        role_prompt.set(String::new());
        worker_profile.set("coding".into());
        model_preference.set("auto".into());
        tools_csv.set(String::new());
        max_budget.set(String::new());
        is_default.set(false);
    };

    rsx! {
        div { class: "mx-auto flex w-full max-w-6xl flex-col gap-6 px-4 py-6 md:px-6",
            div { class: "flex flex-wrap items-start justify-between gap-3",
                div {
                    h1 { class: "text-xl font-semibold tracking-tight text-[var(--fg)]",
                        "Agents & personas"
                    }
                    p { class: "mt-1 max-w-2xl text-sm text-[var(--muted)]",
                        "Define roles for fleet workers — prompts, profiles, and model preferences. "
                        "No LLM is called from this page yet; personas configure future agent runs."
                    }
                }
                div { class: "flex flex-wrap gap-2",
                    button {
                        class: "rounded-lg border border-[var(--border)] bg-[var(--card)] px-3 py-2 text-sm font-medium text-[var(--fg)] hover:bg-[var(--hover)] disabled:opacity-50",
                        disabled: busy() || loading(),
                        onclick: {
                            let workspace_id = workspace_id.clone();
                            move |_| {
                                let workspace_id = workspace_id.clone();
                                spawn(async move {
                                    busy.set(true);
                                    match seed_starters(&workspace_id).await {
                                        Ok((ins, skip)) => {
                                            show_toast().success(format!(
                                                "Seeded {ins} starters ({skip} already present)"
                                            ));
                                            if let Ok(list) = list_personas(&workspace_id).await {
                                                personas.set(list);
                                            }
                                        }
                                        Err(e) => show_toast().error(e.message()),
                                    }
                                    busy.set(false);
                                });
                            }
                        },
                        "Seed starters"
                    }
                    button {
                        class: "rounded-lg bg-[var(--fg)] px-3 py-2 text-sm font-medium text-[var(--bg)] hover:opacity-90 disabled:opacity-50",
                        disabled: busy(),
                        onclick: open_create,
                        "New persona"
                    }
                }
            }

            if loading() {
                p { class: "text-sm text-[var(--muted)]", "Loading personas…" }
            } else if personas().is_empty() {
                div { class: "rounded-xl border border-dashed border-[var(--border)] bg-[var(--card)] p-10 text-center",
                    p { class: "text-sm text-[var(--muted)]",
                        "No personas yet. Seed the starter catalog or create a custom role."
                    }
                }
            } else {
                div { class: "grid gap-3 sm:grid-cols-2 lg:grid-cols-3",
                    for p in personas() {
                        {
                            let p_click = p.clone();
                            let is_sel = selected_id().as_deref() == Some(p.id.as_str());
                            rsx! {
                                button {
                                    key: "{p.id}",
                                    class: if is_sel {
                                        "flex flex-col gap-2 rounded-xl border-2 border-blue-500 bg-[var(--card)] p-4 text-left shadow-sm"
                                    } else {
                                        "flex flex-col gap-2 rounded-xl border border-[var(--border)] bg-[var(--card)] p-4 text-left hover:border-blue-300"
                                    },
                                    onclick: move |_| open_edit(p_click.clone()),
                                    div { class: "flex items-center gap-2",
                                        span { class: "flex h-9 w-9 items-center justify-center rounded-lg bg-neutral-900 text-sm text-white",
                                            "{icon_label(&p.icon)}"
                                        }
                                        div { class: "min-w-0 flex-1",
                                            div { class: "flex items-center gap-2",
                                                span { class: "truncate font-semibold text-[var(--fg)]", "{p.name}" }
                                                if p.is_default {
                                                    span { class: "rounded-full bg-blue-100 px-2 py-0.5 text-[10px] font-semibold uppercase text-blue-700",
                                                        "default"
                                                    }
                                                }
                                            }
                                            p { class: "truncate text-xs text-[var(--muted)]", "{p.slug}" }
                                        }
                                    }
                                    p { class: "line-clamp-2 text-sm text-[var(--muted)]", "{p.description}" }
                                    div { class: "mt-auto flex flex-wrap gap-1.5",
                                        span { class: "rounded-md bg-[var(--hover)] px-2 py-0.5 text-[11px] font-medium text-[var(--fg)]",
                                            "{p.worker_profile}"
                                        }
                                        span { class: "rounded-md bg-[var(--hover)] px-2 py-0.5 text-[11px] font-medium text-[var(--muted)]",
                                            {p.model_preference.clone().unwrap_or_else(|| "—".into())}
                                        }
                                        if p.is_starter {
                                            span { class: "rounded-md bg-emerald-50 px-2 py-0.5 text-[11px] font-medium text-emerald-700",
                                                "starter"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if show_create() || selected_id().is_some() {
                div { class: "rounded-xl border border-[var(--border)] bg-[var(--card)] p-5 shadow-sm",
                    h2 { class: "text-base font-semibold text-[var(--fg)]",
                        if show_create() { "New persona" } else { "Edit persona" }
                    }
                    div { class: "mt-4 grid gap-3 md:grid-cols-2",
                        label { class: "flex flex-col gap-1 text-sm",
                            span { class: "font-medium text-[var(--muted)]", "Name" }
                            input {
                                class: "rounded-lg border border-[var(--border)] bg-[var(--bg)] px-3 py-2 text-[var(--fg)]",
                                value: "{name}",
                                oninput: move |e| name.set(e.value()),
                            }
                        }
                        label { class: "flex flex-col gap-1 text-sm",
                            span { class: "font-medium text-[var(--muted)]", "Worker profile" }
                            input {
                                class: "rounded-lg border border-[var(--border)] bg-[var(--bg)] px-3 py-2 text-[var(--fg)]",
                                value: "{worker_profile}",
                                placeholder: "coding | ops | analysis",
                                oninput: move |e| worker_profile.set(e.value()),
                            }
                        }
                        label { class: "flex flex-col gap-1 text-sm md:col-span-2",
                            span { class: "font-medium text-[var(--muted)]", "Description" }
                            input {
                                class: "rounded-lg border border-[var(--border)] bg-[var(--bg)] px-3 py-2 text-[var(--fg)]",
                                value: "{description}",
                                oninput: move |e| description.set(e.value()),
                            }
                        }
                        label { class: "flex flex-col gap-1 text-sm",
                            span { class: "font-medium text-[var(--muted)]", "Model preference" }
                            input {
                                class: "rounded-lg border border-[var(--border)] bg-[var(--bg)] px-3 py-2 text-[var(--fg)]",
                                value: "{model_preference}",
                                placeholder: "auto | grok | claude | local",
                                oninput: move |e| model_preference.set(e.value()),
                            }
                        }
                        label { class: "flex flex-col gap-1 text-sm",
                            span { class: "font-medium text-[var(--muted)]", "Max budget (USD)" }
                            input {
                                class: "rounded-lg border border-[var(--border)] bg-[var(--bg)] px-3 py-2 text-[var(--fg)]",
                                r#type: "number",
                                step: "0.01",
                                min: "0",
                                placeholder: "optional",
                                value: "{max_budget}",
                                oninput: move |e| max_budget.set(e.value()),
                            }
                        }
                        label { class: "flex flex-col gap-1 text-sm md:col-span-2",
                            span { class: "font-medium text-[var(--muted)]", "Tools" }
                            input {
                                class: "rounded-lg border border-[var(--border)] bg-[var(--bg)] px-3 py-2 text-[var(--fg)]",
                                placeholder: "git, shell, editor",
                                value: "{tools_csv}",
                                oninput: move |e| tools_csv.set(e.value()),
                            }
                        }
                        label { class: "flex items-center gap-2 text-sm md:col-span-2",
                            input {
                                r#type: "checkbox",
                                checked: is_default(),
                                onchange: move |e| is_default.set(e.checked()),
                            }
                            span { class: "text-[var(--fg)]", "Default persona for this workspace" }
                        }
                        label { class: "flex flex-col gap-1 text-sm md:col-span-2",
                            span { class: "font-medium text-[var(--muted)]", "Role prompt" }
                            textarea {
                                class: "min-h-28 rounded-lg border border-[var(--border)] bg-[var(--bg)] px-3 py-2 font-mono text-sm text-[var(--fg)]",
                                value: "{role_prompt}",
                                oninput: move |e| role_prompt.set(e.value()),
                            }
                        }
                    }
                    div { class: "mt-4 flex flex-wrap gap-2",
                        button {
                            class: "rounded-lg bg-[var(--fg)] px-4 py-2 text-sm font-medium text-[var(--bg)] disabled:opacity-50",
                            disabled: busy() || name().trim().is_empty(),
                            onclick: {
                                let workspace_id = workspace_id.clone();
                                move |_| {
                                    let workspace_id = workspace_id.clone();
                                    spawn(async move {
                                        busy.set(true);
                                        let n = name().trim().to_string();
                                        let d = description();
                                        let rp = role_prompt();
                                        let wp = worker_profile().trim().to_string();
                                        let mp = model_preference().trim().to_string();
                                        let tools = csv_to_tools(&tools_csv());
                                        let budget = max_budget().trim().parse::<f64>().ok();
                                        let def = is_default();
                                        if show_create() {
                                            match create_persona(
                                                &workspace_id,
                                                CreatePersonaBody {
                                                    name: n,
                                                    description: Some(d),
                                                    icon: Some("bot".into()),
                                                    role_prompt: Some(rp),
                                                    worker_profile: Some(wp),
                                                    model_preference: Some(mp),
                                                    max_budget_usd: budget,
                                                    tools: Some(tools),
                                                    is_default: Some(def),
                                                },
                                            )
                                            .await
                                            {
                                                Ok(_) => {
                                                    show_toast().success("Persona created");
                                                    show_create.set(false);
                                                    if let Ok(list) = list_personas(&workspace_id).await {
                                                        personas.set(list);
                                                    }
                                                }
                                                Err(e) => show_toast().error(e.message()),
                                            }
                                        } else if let Some(id) = selected_id() {
                                            match update_persona(
                                                &workspace_id,
                                                &id,
                                                PatchPersonaBody {
                                                    name: Some(n),
                                                    description: Some(d),
                                                    role_prompt: Some(rp),
                                                    worker_profile: Some(wp),
                                                    model_preference: Some(mp),
                                                    max_budget_usd: Some(budget),
                                                    tools: Some(tools),
                                                    is_default: Some(def),
                                                },
                                            )
                                            .await
                                            {
                                                Ok(_) => {
                                                    show_toast().success("Persona saved");
                                                    if let Ok(list) = list_personas(&workspace_id).await {
                                                        personas.set(list);
                                                    }
                                                }
                                                Err(e) => show_toast().error(e.message()),
                                            }
                                        }
                                        busy.set(false);
                                    });
                                }
                            },
                            if show_create() { "Create" } else { "Save" }
                        }
                        if let Some(id) = selected_id() {
                            button {
                                class: "rounded-lg border border-red-200 px-4 py-2 text-sm font-medium text-red-700 hover:bg-red-50 disabled:opacity-50",
                                disabled: busy(),
                                onclick: {
                                    let workspace_id = workspace_id.clone();
                                    let id = id.clone();
                                    move |_| {
                                        let workspace_id = workspace_id.clone();
                                        let id = id.clone();
                                        spawn(async move {
                                            busy.set(true);
                                            match archive_persona(&workspace_id, &id).await {
                                                Ok(()) => {
                                                    show_toast().success("Persona archived");
                                                    selected_id.set(None);
                                                    if let Ok(list) = list_personas(&workspace_id).await {
                                                        personas.set(list);
                                                    }
                                                }
                                                Err(e) => show_toast().error(e.message()),
                                            }
                                            busy.set(false);
                                        });
                                    }
                                },
                                "Archive"
                            }
                        }
                        button {
                            class: "rounded-lg border border-[var(--border)] px-4 py-2 text-sm text-[var(--muted)]",
                            onclick: move |_| {
                                show_create.set(false);
                                selected_id.set(None);
                            },
                            "Cancel"
                        }
                    }
                }
            }
        }
    }
}
