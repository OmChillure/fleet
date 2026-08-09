use dioxus::prelude::*;

use super::api::{ensure_fleet, update_fleet_config};
use crate::components::toast::show_toast;

#[component]
pub fn Settings(workspace_id: String) -> Element {
    let mut repo_url = use_signal(|| String::new());
    let mut git_ref = use_signal(|| "main".to_string());
    let mut loading = use_signal(|| true);
    let mut saving = use_signal(|| false);
    let mut load_error = use_signal(|| Option::<String>::None);

    let ws = workspace_id.clone();
    use_effect(move || {
        let ws = ws.clone();
        spawn(async move {
            match ensure_fleet(&ws).await {
                Ok(v) => {
                    repo_url.set(
                        v["config"]["repo_url"]
                            .as_str()
                            .unwrap_or("")
                            .to_string(),
                    );
                    git_ref.set(
                        v["config"]["git_ref"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .unwrap_or("main")
                            .to_string(),
                    );
                    load_error.set(None);
                }
                Err(e) => load_error.set(Some(e.message())),
            }
            loading.set(false);
        });
    });

    let on_save = {
        let workspace_id = workspace_id.clone();
        move |evt: Event<FormData>| {
            evt.prevent_default();
            if saving() {
                return;
            }
            saving.set(true);
            let ws = workspace_id.clone();
            let url = repo_url.read().trim().to_string();
            let rf = git_ref.read().trim().to_string();
            spawn(async move {
                let url_arg = if url.is_empty() { Some("") } else { Some(url.as_str()) };
                let ref_arg = if rf.is_empty() { None } else { Some(rf.as_str()) };
                match update_fleet_config(&ws, url_arg, ref_arg).await {
                    Ok(v) => {
                        repo_url.set(
                            v["config"]["repo_url"]
                                .as_str()
                                .unwrap_or("")
                                .to_string(),
                        );
                        git_ref.set(
                            v["config"]["git_ref"]
                                .as_str()
                                .filter(|s| !s.is_empty())
                                .unwrap_or("main")
                                .to_string(),
                        );
                        show_toast().success(if url.is_empty() {
                            "Cleared project git remote. Tasks will not clone a repo."
                        } else {
                            "Saved project git remote. New tasks will clone this repo."
                        });
                    }
                    Err(e) => show_toast().error(e.message()),
                }
                saving.set(false);
            });
        }
    };

    rsx! {
        div { class: "mx-auto flex h-full w-full max-w-3xl flex-col gap-6 overflow-auto p-4 md:p-5",
            div {
                h1 { class: "m-0 text-2xl font-bold tracking-tight text-[var(--app-fg)]", "Settings" }
                p { class: "mt-1 text-sm text-[var(--app-muted)]",
                    "Workspace preferences. Right now a project maps to one git repo."
                }
            }

            if let Some(err) = load_error() {
                p { class: "rounded-md border border-red-500/30 bg-red-500/10 px-3 py-2 text-sm text-red-400",
                    "{err}"
                }
            }

            form {
                class: "flex flex-col gap-4 rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4 md:p-5",
                onsubmit: on_save,
                h2 { class: "m-0 text-base font-semibold text-[var(--app-fg)]", "Git repository" }
                p { class: "m-0 text-sm text-[var(--app-muted)]",
                    "Agents clone this remote into the sandbox before they start. Multi-repo projects are not supported yet. Leave the URL empty to skip clone (mock / prompt-only tasks)."
                }
                label { class: "flex flex-col gap-1.5 text-sm",
                    span { class: "font-medium text-[var(--app-fg)]", "Clone URL" }
                    input {
                        class: "rounded-md border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 font-mono text-sm text-[var(--app-fg)] outline-none focus:border-[var(--app-accent)]",
                        r#type: "text",
                        name: "repo_url",
                        placeholder: "https://github.com/org/repo.git",
                        disabled: loading() || saving(),
                        value: "{repo_url}",
                        oninput: move |e| repo_url.set(e.value()),
                    }
                }
                label { class: "flex flex-col gap-1.5 text-sm",
                    span { class: "font-medium text-[var(--app-fg)]", "Default ref" }
                    input {
                        class: "rounded-md border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2 font-mono text-sm text-[var(--app-fg)] outline-none focus:border-[var(--app-accent)]",
                        r#type: "text",
                        name: "git_ref",
                        placeholder: "main",
                        disabled: loading() || saving(),
                        value: "{git_ref}",
                        oninput: move |e| git_ref.set(e.value()),
                    }
                    span { class: "text-xs text-[var(--app-muted)]",
                        "Branch, tag, or commit checked out after clone."
                    }
                }
                div { class: "flex justify-end",
                    button {
                        class: "rounded-md bg-[var(--app-accent)] px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50",
                        r#type: "submit",
                        disabled: loading() || saving(),
                        if saving() { "Saving…" } else { "Save repository" }
                    }
                }
            }
        }
    }
}
