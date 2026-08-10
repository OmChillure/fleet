//! App sidebar — collapsible sections with nested links (sidenav07-style, Dioxus).

use dioxus::prelude::*;

use crate::components::toast::show_toast;
use crate::pages::auth::api::{
    CreateWorkspaceBody, UserPublic, WorkspacePublic, create_workspace, list_workspaces, logout,
    me,
};
use crate::theme::{Theme, use_theme};
use crate::Route;

/// Which top-level nav sections are expanded.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum NavSection {
    Chat,
    Monitoring,
    Agents,
    Workspace,
}

#[component]
pub fn Sidebar() -> Element {
    let route = use_route::<Route>();
    let workspace_id = route.workspace_id().unwrap_or("").to_string();

    // Full-width when true; collapsed rail with only the toggle when false.
    let mut sidebar_open = use_signal(|| true);
    let mut workspaces = use_signal(|| Vec::<WorkspacePublic>::new());
    let mut user = use_signal(|| Option::<UserPublic>::None);
    let mut open = use_signal(|| false);
    let mut creating = use_signal(|| false);
    let mut new_name = use_signal(|| String::new());
    let mut create_error = use_signal(|| Option::<String>::None);
    let mut create_loading = use_signal(|| false);
    let mut load_error = use_signal(|| Option::<String>::None);
    let navigator = use_navigator();

    // Collapsible sections — start with Chat + Monitoring open.
    let mut expanded = use_signal(|| {
        std::collections::HashSet::from([NavSection::Chat, NavSection::Monitoring])
    });

    // Ensure the section for the active route stays open while navigating.
    use_effect(move || {
        let section = match use_route::<Route>() {
            Route::Chat { .. }
            | Route::ChatThread { .. }
            | Route::Sessions { .. }
            | Route::SessionDetail { .. } => {
                Some(NavSection::Chat)
            }
            Route::Fleet { .. } | Route::Dashboard { .. } | Route::Sandboxes { .. } => {
                Some(NavSection::Monitoring)
            }
            Route::Agents { .. } => Some(NavSection::Agents),
            Route::Integrations { .. } | Route::Settings { .. } => Some(NavSection::Workspace),
            _ => None,
        };
        if let Some(section) = section {
            let mut set = expanded();
            if set.insert(section) {
                expanded.set(set);
            }
        }
    });

    use_effect(move || {
        spawn(async move {
            // Session user for footer email.
            if let Ok((u, _)) = me().await {
                user.set(Some(u));
            }
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

    let is_open = sidebar_open();

    rsx! {
        aside {
            class: if is_open {
                "flex h-full w-64 shrink-0 flex-col border-r border-[var(--app-border)] bg-[var(--app-sidebar)] text-[var(--app-fg)] transition-[width] duration-200"
            } else {
                "flex h-full w-14 shrink-0 flex-col border-r border-[var(--app-border)] bg-[var(--app-sidebar)] text-[var(--app-fg)] transition-[width] duration-200"
            },

            // Brand + open/close toggle
            div {
                class: if is_open {
                    "flex h-11 items-center gap-1 border-b border-[var(--app-border)] px-2"
                } else {
                    "flex h-11 items-center justify-center border-b border-[var(--app-border)] px-1"
                },
                if is_open {
                    Link {
                        class: "flex min-w-0 flex-1 items-center gap-2 font-semibold tracking-tight text-[var(--app-fg)]",
                        to: Route::Home {},
                        div { class: "h-6 w-6 shrink-0 rounded-md bg-[var(--app-accent)]" }
                        span { class: "truncate text-sm", "Agent Fleet" }
                    }
                }
                button {
                    class: "inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-[var(--app-muted)] transition hover:bg-[var(--app-bg-hover)] hover:text-[var(--app-fg)]",
                    r#type: "button",
                    title: if is_open { "Close sidebar" } else { "Open sidebar" },
                    "aria-label": if is_open { "Close sidebar" } else { "Open sidebar" },
                    onclick: move |_| {
                        let next = !sidebar_open();
                        sidebar_open.set(next);
                        if !next {
                            open.set(false);
                            creating.set(false);
                        }
                    },
                    IconPanelLeft {}
                }
            }

            if is_open {
            // Workspace switcher (no bottom border / separator under create)
            div { class: "relative px-2 pt-2 pb-1",
                button {
                    class: "flex w-full items-center justify-between gap-2 rounded-lg border border-[var(--app-border)] bg-[var(--app-bg-elevated)] px-2.5 py-1.5 text-left text-sm font-medium text-[var(--app-fg)] transition hover:bg-[var(--app-bg-hover)]",
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
                        class: "absolute left-3 right-3 z-20 mt-1 max-h-72 overflow-auto rounded-lg border border-[var(--app-border)] bg-[var(--app-bg-elevated)] py-1 shadow-lg",
                        if let Some(err) = load_error() {
                            p { class: "px-3 py-1.5 text-xs text-red-400", "{err}" }
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
                                            "flex w-full items-center px-3 py-1.5 text-left text-sm font-medium text-[var(--app-fg)] bg-[var(--app-bg-hover)]"
                                        } else {
                                            "flex w-full items-center px-3 py-1.5 text-left text-sm text-[var(--app-muted)] transition hover:bg-[var(--app-bg-hover)] hover:text-[var(--app-fg)]"
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
                                class: "flex w-full items-center px-3 py-1.5 text-left text-sm font-medium text-[var(--app-fg)] transition hover:bg-[var(--app-bg-hover)]",
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

            // Nav sections
            nav { class: "flex min-h-0 flex-1 flex-col gap-0 overflow-y-auto px-2 py-0.5",
                NavAccordion {
                    title: "Chat",
                    expanded: expanded().contains(&NavSection::Chat),
                    on_toggle: move |_| {
                        let mut set = expanded();
                        if !set.remove(&NavSection::Chat) {
                            set.insert(NavSection::Chat);
                        }
                        expanded.set(set);
                    },
                    icon: rsx! { IconMessage {} },
                    NavSubLink {
                        to: Route::Chat { workspace_id: workspace_id.clone() },
                        label: "New chat",
                    }
                    NavSubLink {
                        to: Route::Sessions { workspace_id: workspace_id.clone() },
                        label: "Chat history",
                    }
                }

                NavAccordion {
                    title: "Monitoring",
                    expanded: expanded().contains(&NavSection::Monitoring),
                    on_toggle: move |_| {
                        let mut set = expanded();
                        if !set.remove(&NavSection::Monitoring) {
                            set.insert(NavSection::Monitoring);
                        }
                        expanded.set(set);
                    },
                    icon: rsx! { IconMonitor {} },
                    NavSubLink {
                        to: Route::Dashboard { workspace_id: workspace_id.clone() },
                        label: "Dashboard",
                    }
                    NavSubLink {
                        to: Route::Fleet { workspace_id: workspace_id.clone() },
                        label: "Fleet",
                    }
                    NavSubLink {
                        to: Route::Sandboxes { workspace_id: workspace_id.clone() },
                        label: "Sandboxes",
                    }
                }

                NavAccordion {
                    title: "Agents",
                    expanded: expanded().contains(&NavSection::Agents),
                    on_toggle: move |_| {
                        let mut set = expanded();
                        if !set.remove(&NavSection::Agents) {
                            set.insert(NavSection::Agents);
                        }
                        expanded.set(set);
                    },
                    icon: rsx! { IconBot {} },
                    NavSubLink {
                        to: Route::Agents { workspace_id: workspace_id.clone() },
                        label: "All agents",
                    }
                    NavSubLink {
                        to: Route::Sessions { workspace_id: workspace_id.clone() },
                        label: "Live sessions",
                    }
                }

                NavAccordion {
                    title: "Workspace",
                    expanded: expanded().contains(&NavSection::Workspace),
                    on_toggle: move |_| {
                        let mut set = expanded();
                        if !set.remove(&NavSection::Workspace) {
                            set.insert(NavSection::Workspace);
                        }
                        expanded.set(set);
                    },
                    icon: rsx! { IconSettings {} },
                    NavSubLink {
                        to: Route::Integrations { workspace_id: workspace_id.clone() },
                        label: "Integrations",
                    }
                    NavSubLink {
                        to: Route::Settings { workspace_id: workspace_id.clone() },
                        label: "Settings",
                    }
                }
            }

            // Footer — click opens account dropdown upward
            UserFooter {
                user: user(),
                workspace_id: workspace_id.clone(),
            }
            } // end if is_open
        }
    }
}

/// Bottom user strip: click opens a dropdown menu above (sidenav07-style user menu).
#[component]
fn UserFooter(user: Option<UserPublic>, workspace_id: String) -> Element {
    let mut menu_open = use_signal(|| false);
    let navigator = use_navigator();
    let mut theme = use_theme();
    let is_black = matches!(theme(), Theme::Black);

    let email = user
        .as_ref()
        .map(|u| u.email.clone())
        .unwrap_or_else(|| "…".into());
    let display = user
        .as_ref()
        .and_then(|u| u.display_name.clone())
        .filter(|s| !s.trim().is_empty());
    let initial = user
        .as_ref()
        .map(|u| {
            let src = u
                .display_name
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(u.email.as_str());
            src.chars()
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_else(|| "?".into())
        })
        .unwrap_or_else(|| "?".into());

    let label_primary = display.clone().unwrap_or_else(|| email.clone());
    let label_secondary = if display.is_some() {
        Some(email.clone())
    } else {
        None
    };

    rsx! {
        div { class: "relative mt-auto border-t border-[var(--app-border)] p-1.5",
            // Dropdown panel — opens upward from the footer
            if menu_open() {
                // Click-away backdrop
                div {
                    class: "fixed inset-0 z-30",
                    onclick: move |_| menu_open.set(false),
                }
                div {
                    class: "absolute bottom-full left-2 right-2 z-40 mb-1 overflow-hidden rounded-xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] py-1 shadow-lg",
                    // Header: who is signed in
                    div { class: "border-b border-[var(--app-border)] px-3 py-2.5",
                        p { class: "m-0 truncate text-sm font-medium text-[var(--app-fg)]", "{label_primary}" }
                        if let Some(sec) = label_secondary.clone() {
                            p { class: "m-0 truncate text-xs text-[var(--app-muted)]", "{sec}" }
                        } else {
                            p { class: "m-0 truncate text-xs text-[var(--app-muted)]", "Signed in" }
                        }
                    }

                    // Theme row
                    button {
                        class: "flex w-full items-center justify-between gap-2 px-3 py-2 text-left text-sm text-[var(--app-fg)] transition hover:bg-[var(--app-bg-hover)]",
                        r#type: "button",
                        onclick: move |_| {
                            let next = theme().toggle();
                            theme.set(next);
                        },
                        span { "Appearance" }
                        span { class: "text-xs text-[var(--app-muted)]",
                            if is_black { "Dark" } else { "Light" }
                        }
                    }

                    // Settings
                    {
                        let ws = workspace_id.clone();
                        rsx! {
                            button {
                                class: "flex w-full items-center px-3 py-2 text-left text-sm text-[var(--app-fg)] transition hover:bg-[var(--app-bg-hover)]",
                                r#type: "button",
                                onclick: move |_| {
                                    menu_open.set(false);
                                    navigator.push(Route::Settings {
                                        workspace_id: ws.clone(),
                                    });
                                },
                                "Settings"
                            }
                        }
                    }

                    button {
                        class: "flex w-full items-center px-3 py-2 text-left text-sm text-[var(--app-fg)] transition hover:bg-[var(--app-bg-hover)]",
                        r#type: "button",
                        onclick: move |_| {
                            menu_open.set(false);
                            navigator.push(Route::Home {});
                        },
                        "Back to landing"
                    }

                    div { class: "my-1 border-t border-[var(--app-border)]" }

                    button {
                        class: "flex w-full items-center px-3 py-2 text-left text-sm font-medium text-red-400 transition hover:bg-red-500/10",
                        r#type: "button",
                        onclick: move |_| {
                            menu_open.set(false);
                            let navigator = navigator.clone();
                            spawn(async move {
                                match logout().await {
                                    Ok(()) => {
                                        let _ = navigator.replace(Route::Login {});
                                    }
                                    Err(e) => {
                                        show_toast().error(format!(
                                            "Log out failed: {}",
                                            e.message()
                                        ));
                                    }
                                }
                            });
                        },
                        "Log out"
                    }
                }
            }

            // Trigger row — opens menu upward
            button {
                class: "flex w-full items-center gap-2 rounded-lg p-1.5 text-left transition hover:bg-[var(--app-bg-hover)]",
                r#type: "button",
                "aria-expanded": if menu_open() { "true" } else { "false" },
                "aria-haspopup": "menu",
                onclick: move |_| {
                    let next = !menu_open();
                    menu_open.set(next);
                },
                div {
                    class: "flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-[var(--app-bg-elevated)] text-sm font-semibold text-[var(--app-fg)] ring-1 ring-[var(--app-border)]",
                    "{initial}"
                }
                div { class: "min-w-0 flex-1",
                    p { class: "m-0 truncate text-sm font-medium text-[var(--app-fg)]", "{label_primary}" }
                    if let Some(sec) = label_secondary {
                        p { class: "m-0 truncate text-xs text-[var(--app-muted)]", "{sec}" }
                    }
                }
                span { class: "shrink-0 text-[10px] text-[var(--app-muted)]",
                    if menu_open() { "▾" } else { "▴" }
                }
            }
        }
    }
}

/* ── Accordion section ─────────────────────────────────────────── */

#[component]
fn NavAccordion(
    title: &'static str,
    expanded: bool,
    on_toggle: EventHandler<MouseEvent>,
    icon: Element,
    children: Element,
) -> Element {
    rsx! {
        div { class: "flex flex-col",
            button {
                class: "flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm font-medium text-[var(--app-fg)] transition hover:bg-[var(--app-bg-hover)]",
                r#type: "button",
                onclick: move |e| on_toggle.call(e),
                span { class: "flex h-4 w-4 shrink-0 items-center justify-center text-[var(--app-muted)]",
                    {icon}
                }
                span { class: "min-w-0 flex-1 truncate", "{title}" }
                span {
                    class: "shrink-0 text-[10px] text-[var(--app-muted)] transition-transform",
                    if expanded { "▾" } else { "▸" }
                }
            }
            if expanded {
                div { class: "ml-2.5 flex flex-col border-l border-[var(--app-border)] pl-1.5",
                    {children}
                }
            }
        }
    }
}

#[component]
fn NavSubLink(to: Route, label: &'static str) -> Element {
    rsx! {
        Link {
            class: "block rounded-md px-2 py-1 text-sm text-[var(--app-muted)] transition hover:bg-[var(--app-bg-hover)] hover:text-[var(--app-fg)]",
            to,
            active_class: "bg-[var(--app-bg-elevated)] font-medium text-[var(--app-fg)]",
            "{label}"
        }
    }
}

/* ── Inline icons (no external icon crate) ─────────────────────── */

#[component]
fn IconPanelLeft() -> Element {
    rsx! {
        svg {
            class: "h-4 w-4",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            // Panel-left: rectangle with a left rail
            rect { x: "3", y: "3", width: "18", height: "18", rx: "2" }
            path { d: "M9 3v18" }
        }
    }
}

#[component]
fn IconMessage() -> Element {
    rsx! {
        svg {
            class: "h-4 w-4",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            path { d: "M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" }
        }
    }
}

#[component]
fn IconMonitor() -> Element {
    rsx! {
        svg {
            class: "h-4 w-4",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            rect { x: "2", y: "3", width: "20", height: "14", rx: "2" }
            path { d: "M8 21h8M12 17v4" }
        }
    }
}

#[component]
fn IconBot() -> Element {
    rsx! {
        svg {
            class: "h-4 w-4",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            rect { x: "3", y: "11", width: "18", height: "10", rx: "2" }
            path { d: "M12 2v4M8 16h.01M16 16h.01" }
            circle { cx: "12", cy: "7", r: "1" }
        }
    }
}

#[component]
fn IconSettings() -> Element {
    rsx! {
        svg {
            class: "h-4 w-4",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "2",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            path { d: "M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6z" }
            path { d: "M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" }
        }
    }
}
