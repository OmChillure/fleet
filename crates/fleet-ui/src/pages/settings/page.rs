//! Workspace Settings: concurrent agents + local fleet LLM (Ollama). Grok is env-only.

use dioxus::prelude::*;

use super::api::{FleetSettingsPatch, fetch_settings, save_settings};
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
    let mut effective = use_signal(|| None::<String>);
    let mut ollama_models = use_signal(Vec::<ChatModelInfo>::new);
    let mut ollama_error = use_signal(|| None::<String>);

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
        spawn(async move {
            let s = match save_settings(
                &ws,
                &FleetSettingsPatch {
                    max_agents: n,
                    fleet_llm: llm,
                    fleet_llm_model: model,
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
                "How many agents run at once. They share a workspace and talk to each other — no git URL to set. Connect apps under Integrations only if you want the fleet to act there (GitHub, Slack, …)."
            }

            if loading() {
                p { class: "mt-8 text-sm text-[var(--app-muted)]", "Loading…" }
            } else {
                if let Some(err) = error() {
                    p { class: "mt-4 text-sm text-red-400", "{err}" }
                }

                if grok_env() {
                    p { class: "mt-4 rounded-xl border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-xs leading-5 text-[var(--app-fg)]",
                        "Host has FLEET_AGENT_BACKEND=grok — workers use Grok until that env is unset. The Ollama pick below is saved but not used while Grok env is on."
                    }
                } else if let Some(b) = effective() {
                    p { class: "mt-4 text-xs text-[var(--app-muted)]",
                        "Workers currently use {b}."
                    }
                } else {
                    p { class: "mt-4 text-xs text-[var(--app-muted)]",
                        "Workers are bash-only until you pick a fleet LLM."
                    }
                }

                section { class: "mt-6 rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
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
                        "Local Ollama model for each specialist worker. Grok is not listed here — set FLEET_AGENT_BACKEND=grok on the server."
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
            }
        }
    }
}
