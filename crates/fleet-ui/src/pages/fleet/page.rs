//! Local play UI for fleet-orchestrator: project, fixture, tick, task board.

use dioxus::prelude::*;

use super::api::{
    CreateProjectBody, admin_tick, cancel_latest_run, create_project, fixture_two_task_chain,
    get_project, healthz, list_runs, list_tasks, load_settings, save_settings,
};

#[component]
pub fn Fleet(workspace_id: String) -> Element {
    let _ = &workspace_id; // workspace is auth shell only for now

    let (init_url, init_token, init_project) = load_settings();
    let mut orch_url = use_signal(|| init_url);
    let mut orch_token = use_signal(|| init_token);
    let mut project_id = use_signal(|| init_project);
    let mut project_name = use_signal(|| "demo-fleet".to_string());
    let mut status_line = use_signal(|| "Ready. Configure orchestrator URL/token, then create a project.".to_string());
    let mut health = use_signal(|| "unknown".to_string());
    let mut tasks_json = use_signal(|| "[]".to_string());
    let mut runs_json = use_signal(|| "[]".to_string());
    let mut last_tick = use_signal(|| String::new());
    let mut busy = use_signal(|| false);

    let persist = move || {
        save_settings(&orch_url(), &orch_token(), &project_id());
    };

    rsx! {
        div { class: "mx-auto flex w-full max-w-5xl flex-col gap-4",
            div {
                h1 { class: "m-0 text-2xl font-bold tracking-tight text-[var(--app-fg)]", "Fleet play" }
                p { class: "mt-1 text-sm text-[var(--app-muted)]",
                    "Talks to the orchestrator control plane (mounted on fleet-server at /v1). "
                    "Default: http://localhost:8080 with ADMIN_TOKEN (same origin as auth)."
                }
            }

            // Connection
            section { class: "rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 mb-3 text-sm font-semibold uppercase tracking-wide text-[var(--app-muted)]", "Connection" }
                div { class: "grid gap-3 md:grid-cols-2",
                    label { class: "flex flex-col gap-1 text-xs text-[var(--app-muted)]",
                        "Orchestrator URL"
                        input {
                            class: "rounded-md border border-[var(--app-border)] bg-[var(--app-bg)] px-2.5 py-2 text-sm text-[var(--app-fg)]",
                            value: "{orch_url}",
                            oninput: move |e| orch_url.set(e.value()),
                        }
                    }
                    label { class: "flex flex-col gap-1 text-xs text-[var(--app-muted)]",
                        "Admin token"
                        input {
                            class: "rounded-md border border-[var(--app-border)] bg-[var(--app-bg)] px-2.5 py-2 font-mono text-sm text-[var(--app-fg)]",
                            value: "{orch_token}",
                            oninput: move |e| orch_token.set(e.value()),
                        }
                    }
                }
                div { class: "mt-3 flex flex-wrap items-center gap-2",
                    button {
                        class: "rounded-md bg-[var(--app-accent)] px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50",
                        disabled: busy(),
                        r#type: "button",
                        onclick: move |_| {
                            persist();
                            busy.set(true);
                            let base = orch_url();
                            spawn(async move {
                                match healthz(&base).await {
                                    Ok(v) => {
                                        health.set(v["status"].as_str().unwrap_or("ok").to_string());
                                        status_line.set("Orchestrator reachable.".into());
                                    }
                                    Err(e) => {
                                        health.set("down".into());
                                        status_line.set(e.message());
                                    }
                                }
                                busy.set(false);
                            });
                        },
                        "Ping /healthz"
                    }
                    span { class: "text-xs text-[var(--app-muted)]", "Health: {health}" }
                }
            }

            // Project
            section { class: "rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 mb-3 text-sm font-semibold uppercase tracking-wide text-[var(--app-muted)]", "Project" }
                div { class: "grid gap-3 md:grid-cols-2",
                    label { class: "flex flex-col gap-1 text-xs text-[var(--app-muted)]",
                        "Name (for create)"
                        input {
                            class: "rounded-md border border-[var(--app-border)] bg-[var(--app-bg)] px-2.5 py-2 text-sm text-[var(--app-fg)]",
                            value: "{project_name}",
                            oninput: move |e| project_name.set(e.value()),
                        }
                    }
                    label { class: "flex flex-col gap-1 text-xs text-[var(--app-muted)]",
                        "Project id"
                        input {
                            class: "rounded-md border border-[var(--app-border)] bg-[var(--app-bg)] px-2.5 py-2 font-mono text-sm text-[var(--app-fg)]",
                            value: "{project_id}",
                            oninput: move |e| {
                                project_id.set(e.value());
                                persist();
                            },
                        }
                    }
                }
                div { class: "mt-3 flex flex-wrap gap-2",
                    button {
                        class: "rounded-md border border-[var(--app-border)] px-3 py-1.5 text-sm font-medium text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)] disabled:opacity-50",
                        disabled: busy(),
                        r#type: "button",
                        onclick: move |_| {
                            persist();
                            busy.set(true);
                            let base = orch_url();
                            let token = orch_token();
                            let name = project_name();
                            spawn(async move {
                                match create_project(
                                    &base,
                                    &token,
                                    CreateProjectBody {
                                        name,
                                        max_agents: 3,
                                    },
                                )
                                .await
                                {
                                    Ok(v) => {
                                        if let Some(id) = v["id"].as_str() {
                                            project_id.set(id.to_string());
                                            save_settings(&base, &token, id);
                                        }
                                        status_line.set(format!("Created project {}", v["id"]));
                                    }
                                    Err(e) => status_line.set(e.message()),
                                }
                                busy.set(false);
                            });
                        },
                        "Create project"
                    }
                    button {
                        class: "rounded-md border border-[var(--app-border)] px-3 py-1.5 text-sm font-medium text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)] disabled:opacity-50",
                        disabled: busy() || project_id().is_empty(),
                        r#type: "button",
                        onclick: move |_| {
                            persist();
                            busy.set(true);
                            let base = orch_url();
                            let token = orch_token();
                            let pid = project_id();
                            spawn(async move {
                                match get_project(&base, &token, &pid).await {
                                    Ok(v) => status_line.set(format!("Project {} ({})", v["name"], v["id"])),
                                    Err(e) => status_line.set(e.message()),
                                }
                                busy.set(false);
                            });
                        },
                        "Load project"
                    }
                }
            }

            // Actions
            section { class: "rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                h2 { class: "m-0 mb-3 text-sm font-semibold uppercase tracking-wide text-[var(--app-muted)]", "Run" }
                div { class: "flex flex-wrap gap-2",
                    button {
                        class: "rounded-md bg-[var(--app-accent)] px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50",
                        disabled: busy() || project_id().is_empty(),
                        r#type: "button",
                        onclick: move |_| {
                            persist();
                            busy.set(true);
                            let base = orch_url();
                            let token = orch_token();
                            let pid = project_id();
                            spawn(async move {
                                match fixture_two_task_chain(&base, &token, &pid).await {
                                    Ok(v) => status_line.set(format!(
                                        "Fixture run {} plan {}",
                                        v["run_id"], v["plan_id"]
                                    )),
                                    Err(e) => status_line.set(e.message()),
                                }
                                busy.set(false);
                            });
                        },
                        "Start 2-task fixture"
                    }
                    button {
                        class: "rounded-md bg-emerald-600 px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50",
                        disabled: busy() || project_id().is_empty(),
                        r#type: "button",
                        onclick: move |_| {
                            persist();
                            busy.set(true);
                            let base = orch_url();
                            let token = orch_token();
                            let pid = project_id();
                            spawn(async move {
                                match admin_tick(&base, &token, &pid).await {
                                    Ok(v) => {
                                        last_tick.set(
                                            serde_json::to_string_pretty(&v).unwrap_or_default(),
                                        );
                                        status_line.set(format!(
                                            "Tick {} — spawned={} completed={} terminated={}",
                                            v["tick_id"],
                                            v["spawned"],
                                            v["completed_via_runtime"],
                                            v["runs_terminated"]
                                        ));
                                    }
                                    Err(e) => status_line.set(e.message()),
                                }
                                // refresh board
                                if let Ok(t) = list_tasks(&base, &token, &pid).await {
                                    tasks_json.set(
                                        serde_json::to_string_pretty(&t["tasks"])
                                            .unwrap_or_else(|_| "[]".into()),
                                    );
                                }
                                if let Ok(r) = list_runs(&base, &token, &pid).await {
                                    runs_json.set(
                                        serde_json::to_string_pretty(&r["runs"])
                                            .unwrap_or_else(|_| "[]".into()),
                                    );
                                }
                                busy.set(false);
                            });
                        },
                        "Run tick"
                    }
                    button {
                        class: "rounded-md border border-[var(--app-border)] px-3 py-1.5 text-sm font-medium text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)] disabled:opacity-50",
                        disabled: busy() || project_id().is_empty(),
                        r#type: "button",
                        onclick: move |_| {
                            persist();
                            busy.set(true);
                            let base = orch_url();
                            let token = orch_token();
                            let pid = project_id();
                            spawn(async move {
                                match list_tasks(&base, &token, &pid).await {
                                    Ok(t) => {
                                        tasks_json.set(
                                            serde_json::to_string_pretty(&t["tasks"])
                                                .unwrap_or_else(|_| "[]".into()),
                                        );
                                        status_line.set("Refreshed tasks.".into());
                                    }
                                    Err(e) => status_line.set(e.message()),
                                }
                                if let Ok(r) = list_runs(&base, &token, &pid).await {
                                    runs_json.set(
                                        serde_json::to_string_pretty(&r["runs"])
                                            .unwrap_or_else(|_| "[]".into()),
                                    );
                                }
                                busy.set(false);
                            });
                        },
                        "Refresh board"
                    }
                    button {
                        class: "rounded-md border border-red-500/40 px-3 py-1.5 text-sm font-medium text-red-400 hover:bg-red-500/10 disabled:opacity-50",
                        disabled: busy() || project_id().is_empty(),
                        r#type: "button",
                        onclick: move |_| {
                            persist();
                            busy.set(true);
                            let base = orch_url();
                            let token = orch_token();
                            let pid = project_id();
                            spawn(async move {
                                match cancel_latest_run(&base, &token, &pid).await {
                                    Ok(v) => status_line.set(format!("Cancel requested: {v}")),
                                    Err(e) => status_line.set(e.message()),
                                }
                                busy.set(false);
                            });
                        },
                        "Cancel latest run"
                    }
                }
                p { class: "mt-3 text-sm text-[var(--app-fg)]", "{status_line}" }
            }

            // Board
            section { class: "grid gap-4 md:grid-cols-2",
                div { class: "rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                    h2 { class: "m-0 mb-2 text-sm font-semibold uppercase tracking-wide text-[var(--app-muted)]", "Tasks" }
                    pre { class: "max-h-80 overflow-auto whitespace-pre-wrap break-all rounded-md bg-[var(--app-bg)] p-3 font-mono text-xs text-[var(--app-fg)]",
                        "{tasks_json}"
                    }
                }
                div { class: "rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
                    h2 { class: "m-0 mb-2 text-sm font-semibold uppercase tracking-wide text-[var(--app-muted)]", "Runs / last tick" }
                    pre { class: "mb-3 max-h-40 overflow-auto whitespace-pre-wrap break-all rounded-md bg-[var(--app-bg)] p-3 font-mono text-xs text-[var(--app-fg)]",
                        "{runs_json}"
                    }
                    pre { class: "max-h-40 overflow-auto whitespace-pre-wrap break-all rounded-md bg-[var(--app-bg)] p-3 font-mono text-xs text-[var(--app-muted)]",
                        "{last_tick}"
                    }
                }
            }

            p { class: "text-xs text-[var(--app-muted)]",
                "Tip: Start fixture, then press "
                strong { "Run tick" }
                " a few times until tasks are done_success and the run is completed."
            }
        }
    }
}
