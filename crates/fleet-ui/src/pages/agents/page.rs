use dioxus::prelude::*;

use super::api::{PersonaDto, fetch_personas};

#[component]
pub fn Agents(workspace_id: String) -> Element {
    let mut loading = use_signal(|| true);
    let mut error = use_signal(|| None::<String>);
    let mut personas = use_signal(Vec::<PersonaDto>::new);
    let mut backend = use_signal(|| None::<String>);
    let mut source = use_signal(String::new);
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
            loading.set(false);
        });
    });

    rsx! {
        div { class: "mx-auto max-w-5xl px-4 py-6 md:px-6",
            h1 { class: "m-0 text-2xl font-semibold tracking-tight text-[var(--app-fg)]",
                "Agents"
            }
            p { class: "mt-1 text-sm text-[var(--app-muted)]",
                "Specialist personas. Launch a plan with different slugs — each agent sees TEAM.md, talks mid-run, shares `work/`, and reviewers can send work back. "
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
        }
    }
}
