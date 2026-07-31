use dioxus::prelude::*;

use crate::pages::auth::api::{
    CreateWorkspaceBody, WorkspacePublic, create_workspace, list_workspaces,
};
use crate::Route;

#[component]
pub fn Sidebar() -> Element {
    let route = use_route::<Route>();
    let workspace_id = route.workspace_id().unwrap_or("").to_string();

    let mut workspaces = use_signal(|| Vec::<WorkspacePublic>::new());
    let mut open = use_signal(|| false);
    let mut creating = use_signal(|| false);
    let mut new_name = use_signal(|| String::new());
    let mut create_error = use_signal(|| Option::<String>::None);
    let mut create_loading = use_signal(|| false);
    let mut load_error = use_signal(|| Option::<String>::None);
    let navigator = use_navigator();

    use_effect(move || {
        spawn(async move {
            match list_workspaces().await {
                Ok(list) => {
                    workspaces.set(list);
                    load_error.set(None);
                }
                Err(e) => {
                    load_error.set(Some(e.message()));
                }
            }
        });
    });

    let list_snapshot = workspaces();
    let current = list_snapshot
        .iter()
        .find(|w| w.id == workspace_id)
        .cloned()
        .or_else(|| list_snapshot.first().cloned());

    let current_label = current
        .as_ref()
        .map(|w| w.name.clone())
        .unwrap_or_else(|| {
            if workspace_id.is_empty() {
                "Select workspace".into()
            } else {
                "Workspace".into()
            }
        });

    rsx! {
        aside {
            class: "flex h-full w-56 shrink-0 flex-col border-r border-[var(--app-border)] bg-[var(--app-bg)]",
            div { class: "flex h-12 items-center gap-2 border-b border-[var(--app-border)] px-3",
                Link {
                    class: "flex items-center gap-2 font-semibold tracking-tight text-[var(--app-fg)]",
                    to: Route::Home {},
                    div { class: "h-6 w-6 rounded-md bg-[var(--app-accent)]" }
                    span { class: "text-sm", "Agent Fleet" }
                }
            }

            // Workspace switcher — above Dashboard
            div { class: "relative border-b border-[var(--app-border)] p-2",
                button {
                    class: "flex w-full items-center justify-between gap-2 rounded-md border border-[var(--app-border)] bg-[var(--app-bg-elevated)] px-2.5 py-2 text-left text-sm font-medium text-[var(--app-fg)] transition hover:bg-[var(--app-bg-hover)]",
                    r#type: "button",
                    onclick: move |_| {
                        let next = !open();
                        open.set(next);
                        if !next {
                            creating.set(false);
                            create_error.set(None);
                        }
                    },
                    span { class: "min-w-0 flex-1 truncate", "{current_label}" }
                    span { class: "shrink-0 text-[10px] text-[var(--app-muted)]",
                        if open() { "▲" } else { "▼" }
                    }
                }

                if open() {
                    div {
                        class: "absolute left-2 right-2 z-20 mt-1 max-h-72 overflow-auto rounded-md border border-[var(--app-border)] bg-[var(--app-bg-elevated)] py-1 shadow-lg",
                        if let Some(err) = load_error() {
                            p { class: "px-2.5 py-1.5 text-xs text-red-400", "{err}" }
                        }
                        for ws in workspaces() {
                            {
                                let id = ws.id.clone();
                                let name = ws.name.clone();
                                let selected = id == workspace_id;
                                let navigator = navigator.clone();
                                rsx! {
                                    button {
                                        key: "{id}",
                                        class: if selected {
                                            "flex w-full items-center px-2.5 py-1.5 text-left text-sm font-medium text-[var(--app-fg)] bg-[var(--app-bg-hover)]"
                                        } else {
                                            "flex w-full items-center px-2.5 py-1.5 text-left text-sm text-[var(--app-muted)] transition hover:bg-[var(--app-bg-hover)] hover:text-[var(--app-fg)]"
                                        },
                                        r#type: "button",
                                        onclick: move |_| {
                                            open.set(false);
                                            creating.set(false);
                                            navigator.push(Route::Dashboard {
                                                workspace_id: id.clone(),
                                            });
                                        },
                                        span { class: "truncate", "{name}" }
                                    }
                                }
                            }
                        }

                        div { class: "my-1 border-t border-[var(--app-border)]" }

                        if creating() {
                            div { class: "space-y-1.5 px-2 py-1.5",
                                input {
                                    class: "w-full rounded-md border border-[var(--app-border)] bg-[var(--app-bg)] px-2 py-1.5 text-sm text-[var(--app-fg)] outline-none focus:border-[var(--app-muted)]",
                                    r#type: "text",
                                    placeholder: "Workspace name",
                                    value: "{new_name}",
                                    oninput: move |e| new_name.set(e.value()),
                                }
                                if let Some(err) = create_error() {
                                    p { class: "text-xs text-red-400", "{err}" }
                                }
                                div { class: "flex gap-1",
                                    button {
                                        class: "flex-1 rounded-md bg-[var(--app-fg)] px-2 py-1 text-xs font-semibold text-[var(--app-bg)] disabled:opacity-60",
                                        r#type: "button",
                                        disabled: create_loading(),
                                        onclick: move |_| {
                                            if create_loading() {
                                                return;
                                            }
                                            let name = new_name.read().trim().to_string();
                                            if name.is_empty() {
                                                create_error.set(Some("Name is required.".into()));
                                                return;
                                            }
                                            create_loading.set(true);
                                            create_error.set(None);
                                            let navigator = navigator.clone();
                                            spawn(async move {
                                                match create_workspace(CreateWorkspaceBody {
                                                    name,
                                                    slug: None,
                                                })
                                                .await
                                                {
                                                    Ok(ws) => {
                                                        let mut list = workspaces();
                                                        list.push(ws.clone());
                                                        workspaces.set(list);
                                                        new_name.set(String::new());
                                                        creating.set(false);
                                                        open.set(false);
                                                        create_loading.set(false);
                                                        navigator.push(Route::Dashboard {
                                                            workspace_id: ws.id,
                                                        });
                                                    }
                                                    Err(e) => {
                                                        create_loading.set(false);
                                                        create_error.set(Some(e.message()));
                                                    }
                                                }
                                            });
                                        },
                                        if create_loading() { "Creating…" } else { "Create" }
                                    }
                                    button {
                                        class: "rounded-md px-2 py-1 text-xs text-[var(--app-muted)] hover:text-[var(--app-fg)]",
                                        r#type: "button",
                                        onclick: move |_| {
                                            creating.set(false);
                                            create_error.set(None);
                                            new_name.set(String::new());
                                        },
                                        "Cancel"
                                    }
                                }
                            }
                        } else {
                            button {
                                class: "flex w-full items-center px-2.5 py-1.5 text-left text-sm font-medium text-[var(--app-fg)] transition hover:bg-[var(--app-bg-hover)]",
                                r#type: "button",
                                onclick: move |_| {
                                    creating.set(true);
                                    create_error.set(None);
                                },
                                "+ Create workspace"
                            }
                        }
                    }
                }
            }

            nav { class: "flex flex-1 flex-col gap-0.5 p-2",
                SidebarLink {
                    to: Route::Dashboard { workspace_id: workspace_id.clone() },
                    label: "Dashboard",
                }
                SidebarLink {
                    to: Route::Sessions { workspace_id: workspace_id.clone() },
                    label: "Sessions",
                }
                SidebarLink {
                    to: Route::Agents { workspace_id: workspace_id.clone() },
                    label: "Agents",
                }
                SidebarLink {
                    to: Route::Sandboxes { workspace_id: workspace_id.clone() },
                    label: "Sandboxes",
                }
                SidebarLink {
                    to: Route::Integrations { workspace_id: workspace_id.clone() },
                    label: "Integrations",
                }
                SidebarLink {
                    to: Route::Settings { workspace_id: workspace_id.clone() },
                    label: "Settings",
                }
            }

            div { class: "border-t border-[var(--app-border)] p-2 text-xs text-[var(--app-muted)]",
                Link {
                    class: "block rounded-md px-2 py-1.5 transition hover:bg-[var(--app-bg-hover)] hover:text-[var(--app-fg)]",
                    to: Route::Home {},
                    "← Back to landing"
                }
            }
        }
    }
}

#[component]
fn SidebarLink(to: Route, label: &'static str) -> Element {
    rsx! {
        Link {
            class: "rounded-md px-3 py-2 text-sm font-medium text-[var(--app-muted)] transition hover:bg-[var(--app-bg-hover)] hover:text-[var(--app-fg)]",
            to,
            active_class: "bg-[var(--app-bg-elevated)] text-[var(--app-fg)]",
            "{label}"
        }
    }
}
