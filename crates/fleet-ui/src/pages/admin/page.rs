//! Instance admin: users, runners, workspaces, health.

use dioxus::prelude::*;
use serde_json::Value;

use super::api::{
    AdminUser, AdminWorkspace, Overview, PatchUserBody, fetch_instance, fetch_overview,
    fetch_runners, fetch_sessions, fetch_users, fetch_work, fetch_workspaces, patch_user,
};
use crate::components::toast::show_toast;

fn apply_user_patch(
    user_id: String,
    body: PatchUserBody,
    ok_msg: &'static str,
    on_changed: EventHandler<()>,
) {
    spawn(async move {
        match patch_user(&user_id, &body).await {
            Ok(_) => {
                show_toast().success(ok_msg);
                on_changed.call(());
            }
            Err(e) => show_toast().error(e),
        }
    });
}

fn map_get(m: &std::collections::BTreeMap<String, i64>, key: &str) -> i64 {
    m.get(key).copied().unwrap_or(0)
}

fn json_i64(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(|x| x.as_i64()).unwrap_or(0)
}

fn json_str<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("—")
}

fn json_bool(v: &Value, key: &str) -> bool {
    v.get(key).and_then(|x| x.as_bool()).unwrap_or(false)
}

fn fmt_counts(v: &Value) -> String {
    match v.as_object() {
        Some(obj) if obj.is_empty() => "none".into(),
        Some(obj) => obj
            .iter()
            .map(|(k, n)| format!("{k} {}", n.as_i64().unwrap_or(0)))
            .collect::<Vec<_>>()
            .join(" · "),
        None => "none".into(),
    }
}

fn fmt_true_keys(v: &Value) -> String {
    match v.as_object() {
        Some(obj) => {
            let on: Vec<&str> = obj
                .iter()
                .filter(|(_, n)| n.as_bool() == Some(true))
                .map(|(k, _)| k.as_str())
                .collect();
            if on.is_empty() {
                "none".into()
            } else {
                on.join(", ")
            }
        }
        None => "none".into(),
    }
}

fn short_time(raw: &str) -> String {
    if raw.len() >= 16 {
        raw[..16].replace('T', " ")
    } else {
        raw.to_string()
    }
}

#[component]
pub fn Admin() -> Element {
    let mut loading = use_signal(|| true);
    let mut forbidden = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    let mut overview = use_signal(Overview::default);
    let mut users = use_signal(Vec::<AdminUser>::new);
    let mut you_id = use_signal(String::new);
    let mut workspaces = use_signal(Vec::<AdminWorkspace>::new);
    let mut runners = use_signal(Vec::<Value>::new);
    let mut instance = use_signal(|| Value::Null);
    let mut work = use_signal(|| Value::Null);
    let mut sessions = use_signal(Vec::<Value>::new);
    let mut load_gen = use_signal(|| 0u32);

    use_effect(move || {
        let _ = load_gen();
        spawn(async move {
            match fetch_overview().await {
                Ok(o) => {
                    overview.set(o);
                    forbidden.set(false);
                    error.set(None);
                }
                Err(e) => {
                    if e.contains("instance admin") || e.contains("403") || e.contains("forbidden")
                    {
                        forbidden.set(true);
                    }
                    error.set(Some(e));
                    loading.set(false);
                    return;
                }
            }
            if let Ok(u) = fetch_users().await {
                you_id.set(u.you.id);
                users.set(u.users);
            }
            if let Ok(w) = fetch_workspaces().await {
                workspaces.set(w);
            }
            if let Ok(r) = fetch_runners().await {
                runners.set(r);
            }
            if let Ok(i) = fetch_instance().await {
                instance.set(i);
            }
            if let Ok(w) = fetch_work().await {
                work.set(w);
            }
            if let Ok(s) = fetch_sessions().await {
                sessions.set(s);
            }
            loading.set(false);
        });
    });

    rsx! {
        div { class: "h-full overflow-y-auto",
            div { class: "mx-auto max-w-6xl px-4 py-6 md:px-6",
                h1 { class: "m-0 text-2xl font-semibold tracking-tight text-[var(--app-fg)]",
                    "Instance"
                }
                p { class: "mt-1 text-sm text-[var(--app-muted)]",
                    "Users, runners, and health for this Agent Fleet instance. Workspace members and pairing stay on those workspace pages."
                }

                if loading() {
                    p { class: "mt-8 text-sm text-[var(--app-muted)]", "Loading…" }
                } else if forbidden() {
                    p { class: "mt-8 rounded-xl border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-sm text-[var(--app-fg)]",
                        "Instance admin required. Set INSTANCE_ADMIN_EMAILS to your email and log in again, or ask an existing instance admin to promote you."
                    }
                } else {
                    if let Some(err) = error() {
                        p { class: "mt-4 text-sm text-red-400", "{err}" }
                    }

                    OverviewCards { overview: overview() }
                    UsersSection {
                        users: users(),
                        you_id: you_id(),
                        env_emails: instance()
                            .get("instance_admin_emails")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        on_changed: move |_| {
                            load_gen.set(load_gen() + 1);
                        },
                    }
                    WorkspacesSection { workspaces: workspaces() }
                    RunnersSection { runners: runners() }
                    WorkSection { work: work(), sessions: sessions() }
                    InstanceSection { instance: instance() }
                }
            }
        }
    }
}

#[component]
fn OverviewCards(overview: Overview) -> Element {
    let o = overview;
    let active = map_get(&o.users.by_status, "active");
    let disabled = map_get(&o.users.by_status, "disabled");
    let online = map_get(&o.runners, "online");
    let total_runners = map_get(&o.runners, "total");
    let assignable = map_get(&o.runners, "assignable");
    let ready = map_get(&o.work_claims, "ready");
    let live = map_get(&o.sessions, "healthy") + map_get(&o.sessions, "spawning");
    rsx! {
        section { class: "mt-6 grid gap-3 sm:grid-cols-2 lg:grid-cols-4",
            StatCard { label: "Users", value: "{o.users.total}", hint: "{active} active · {disabled} disabled · {o.users.instance_admins} instance admins" }
            StatCard { label: "Workspaces", value: "{o.workspaces}", hint: "{o.pending_invites} pending invites" }
            StatCard { label: "Runners", value: "{total_runners}", hint: "{online} online · {assignable} assignable" }
            StatCard { label: "Live work", value: "{live}", hint: "{ready} claims ready · {o.connected_integrations} connected apps" }
        }
    }
}

#[component]
fn StatCard(label: String, value: String, hint: String) -> Element {
    rsx! {
        div { class: "rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
            p { class: "m-0 text-xs uppercase tracking-wide text-[var(--app-muted)]", "{label}" }
            p { class: "m-0 mt-1 text-2xl font-semibold text-[var(--app-fg)]", "{value}" }
            p { class: "m-0 mt-1 text-xs text-[var(--app-muted)]", "{hint}" }
        }
    }
}

#[component]
fn UsersSection(
    users: Vec<AdminUser>,
    you_id: String,
    env_emails: Vec<String>,
    on_changed: EventHandler<()>,
) -> Element {
    rsx! {
        section { class: "mt-8 rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
            h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Users" }
            p { class: "mt-1 text-xs text-[var(--app-muted)]",
                "Newest 200 accounts. Promote or disable here. Workspace roles stay under Settings."
            }
            div { class: "mt-3 overflow-x-auto",
                table { class: "w-full min-w-[40rem] border-collapse text-left text-sm",
                    thead {
                        tr { class: "border-b border-[var(--app-border)] text-xs text-[var(--app-muted)]",
                            th { class: "py-2 pr-3 font-medium", "Email" }
                            th { class: "py-2 pr-3 font-medium", "Status" }
                            th { class: "py-2 pr-3 font-medium", "Workspaces" }
                            th { class: "py-2 pr-3 font-medium", "Sessions" }
                            th { class: "py-2 pr-3 font-medium", "Last seen" }
                            th { class: "py-2 font-medium", "" }
                        }
                    }
                    tbody {
                        for u in users {
                            {
                                let id = u.id.clone();
                                let is_you = id == you_id;
                                let env_locked = env_emails.iter().any(|e| e == &u.email.to_lowercase());
                                let is_admin = u.instance_admin;
                                let is_disabled = u.status == "disabled";
                                let last = u
                                    .last_seen_at
                                    .as_deref()
                                    .map(short_time)
                                    .unwrap_or_else(|| "—".into());
                                let name = u.display_name.clone().unwrap_or_default();
                                rsx! {
                                    tr {
                                        key: "{u.id}",
                                        class: "border-b border-[var(--app-border)]/70",
                                        td { class: "py-2 pr-3 align-top",
                                            p { class: "m-0 text-[var(--app-fg)]", "{u.email}" }
                                            p { class: "m-0 text-[11px] text-[var(--app-muted)]",
                                                if !name.is_empty() { "{name} · " }
                                                if is_admin { "instance admin" } else { "user" }
                                                if is_you { " · you" }
                                                if env_locked { " · env" }
                                            }
                                        }
                                        td { class: "py-2 pr-3 align-top text-[var(--app-muted)]", "{u.status}" }
                                        td { class: "py-2 pr-3 align-top text-[var(--app-muted)]", "{u.workspace_count}" }
                                        td { class: "py-2 pr-3 align-top text-[var(--app-muted)]", "{u.active_sessions}" }
                                        td { class: "py-2 pr-3 align-top text-[var(--app-muted)]", "{last}" }
                                        td { class: "py-2 align-top",
                                            div { class: "flex flex-wrap gap-1",
                                                if is_admin {
                                                    button {
                                                        class: "rounded-md border border-[var(--app-border)] px-2 py-1 text-[11px] text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)] disabled:opacity-40",
                                                        r#type: "button",
                                                        disabled: env_locked,
                                                        onclick: {
                                                            let id = id.clone();
                                                            move |_| {
                                                                apply_user_patch(
                                                                    id.clone(),
                                                                    PatchUserBody {
                                                                        instance_admin: Some(false),
                                                                        status: None,
                                                                    },
                                                                    "Demoted instance admin.",
                                                                    on_changed,
                                                                );
                                                            }
                                                        },
                                                        "Demote"
                                                    }
                                                } else {
                                                    button {
                                                        class: "rounded-md border border-[var(--app-border)] px-2 py-1 text-[11px] text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)]",
                                                        r#type: "button",
                                                        onclick: {
                                                            let id = id.clone();
                                                            move |_| {
                                                                apply_user_patch(
                                                                    id.clone(),
                                                                    PatchUserBody {
                                                                        instance_admin: Some(true),
                                                                        status: None,
                                                                    },
                                                                    "Promoted to instance admin.",
                                                                    on_changed,
                                                                );
                                                            }
                                                        },
                                                        "Promote"
                                                    }
                                                }
                                                if is_disabled {
                                                    button {
                                                        class: "rounded-md border border-[var(--app-border)] px-2 py-1 text-[11px] text-[var(--app-fg)] hover:bg-[var(--app-bg-hover)]",
                                                        r#type: "button",
                                                        onclick: {
                                                            let id = id.clone();
                                                            move |_| {
                                                                apply_user_patch(
                                                                    id.clone(),
                                                                    PatchUserBody {
                                                                        instance_admin: None,
                                                                        status: Some("active".into()),
                                                                    },
                                                                    "User enabled.",
                                                                    on_changed,
                                                                );
                                                            }
                                                        },
                                                        "Enable"
                                                    }
                                                } else {
                                                    button {
                                                        class: "rounded-md border border-red-500/40 px-2 py-1 text-[11px] text-red-400 hover:bg-red-500/10 disabled:opacity-40",
                                                        r#type: "button",
                                                        disabled: is_you,
                                                        onclick: {
                                                            let id = id.clone();
                                                            move |_| {
                                                                apply_user_patch(
                                                                    id.clone(),
                                                                    PatchUserBody {
                                                                        instance_admin: None,
                                                                        status: Some("disabled".into()),
                                                                    },
                                                                    "User disabled.",
                                                                    on_changed,
                                                                );
                                                            }
                                                        },
                                                        "Disable"
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
        }
    }
}

#[component]
fn WorkspacesSection(workspaces: Vec<AdminWorkspace>) -> Element {
    rsx! {
        section { class: "mt-8 rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
            h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Workspaces" }
            div { class: "mt-3 overflow-x-auto",
                table { class: "w-full min-w-[36rem] border-collapse text-left text-sm",
                    thead {
                        tr { class: "border-b border-[var(--app-border)] text-xs text-[var(--app-muted)]",
                            th { class: "py-2 pr-3 font-medium", "Name" }
                            th { class: "py-2 pr-3 font-medium", "Owners" }
                            th { class: "py-2 pr-3 font-medium", "Members" }
                            th { class: "py-2 pr-3 font-medium", "Runners" }
                            th { class: "py-2 pr-3 font-medium", "Isolation" }
                            th { class: "py-2 font-medium", "Apps" }
                        }
                    }
                    tbody {
                        for w in workspaces {
                            tr {
                                key: "{w.id}",
                                class: "border-b border-[var(--app-border)]/70",
                                td { class: "py-2 pr-3 align-top",
                                    p { class: "m-0 text-[var(--app-fg)]", "{w.name}" }
                                    p { class: "m-0 text-[11px] text-[var(--app-muted)]", "{w.slug}" }
                                }
                                td { class: "py-2 pr-3 align-top text-[var(--app-muted)]",
                                    {w.owners.clone().unwrap_or_else(|| "—".into())}
                                }
                                td { class: "py-2 pr-3 align-top text-[var(--app-muted)]", "{w.member_count}" }
                                td { class: "py-2 pr-3 align-top text-[var(--app-muted)]",
                                    "{w.online_runners}/{w.runner_count}"
                                }
                                td { class: "py-2 pr-3 align-top text-[var(--app-muted)]", "{w.min_isolation}" }
                                td { class: "py-2 align-top text-[var(--app-muted)]",
                                    "{w.connected_integrations}"
                                    if w.inbox_monitor { " · inbox" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn RunnersSection(runners: Vec<Value>) -> Element {
    rsx! {
        section { class: "mt-8 rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
            h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Runners" }
            p { class: "mt-1 text-xs text-[var(--app-muted)]",
                "Newest 200 paired hosts. Pair tokens stay on the workspace Self-hosted page."
            }
            if runners.is_empty() {
                p { class: "mt-3 text-sm text-[var(--app-muted)]", "No runners registered." }
            } else {
                ul { class: "mt-3 m-0 list-none space-y-2 p-0",
                    for r in runners {
                        {
                            let id = json_str(&r, "id").to_string();
                            let name = json_str(&r, "name");
                            let status = json_str(&r, "status");
                            let ws = json_str(&r, "workspace_name");
                            let host = json_str(&r, "hostname");
                            let platform = json_str(&r, "platform");
                            let jobs = json_i64(&r, "current_jobs");
                            let max = json_i64(&r, "max_concurrent");
                            let sw = json_str(&r, "software_status");
                            let paused = json_bool(&r, "paused");
                            let drain = json_bool(&r, "drain");
                            let auto = json_bool(&r, "auto_update");
                            let last = r
                                .get("last_seen_at")
                                .and_then(|v| v.as_str())
                                .map(short_time)
                                .unwrap_or_else(|| "never".into());
                            rsx! {
                                li {
                                    key: "{id}",
                                    class: "rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2",
                                    div { class: "flex flex-wrap items-baseline justify-between gap-2",
                                        p { class: "m-0 text-sm font-medium text-[var(--app-fg)]", "{name}" }
                                        p { class: "m-0 text-xs text-[var(--app-muted)]", "{status}" }
                                    }
                                    p { class: "m-0 mt-1 text-[11px] text-[var(--app-muted)]",
                                        "{ws} · {host} · {platform} · jobs {jobs}/{max} · {sw} · last {last}"
                                        if paused { " · paused" }
                                        if drain { " · drain" }
                                        if !auto { " · auto-update off" }
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

#[component]
fn WorkSection(work: Value, sessions: Vec<Value>) -> Element {
    let claims = work.get("work_claims").cloned().unwrap_or(Value::Null);
    let cases = work.get("cases").cloned().unwrap_or(Value::Null);
    let goals = work.get("goals").cloned().unwrap_or(Value::Null);
    let inbox = work.get("inbox").cloned().unwrap_or(Value::Null);
    let orch = work.get("sessions").cloned().unwrap_or(Value::Null);
    rsx! {
        section { class: "mt-8 rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
            h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Live work" }
            p { class: "mt-1 text-xs text-[var(--app-muted)]",
                "Cross-workspace totals. Open a workspace Fleet or Inbox page for a specific run."
            }
            div { class: "mt-3 grid gap-3 sm:grid-cols-2 lg:grid-cols-4",
                CountBlock { title: "Sessions", value: orch }
                CountBlock { title: "Claims", value: claims }
                CountBlock { title: "Cases", value: cases }
                CountBlock { title: "Goals", value: goals }
            }
            p { class: "mt-3 text-xs text-[var(--app-muted)]",
                "Inbox: {fmt_counts(&inbox)} · monitors {json_i64(&work, \"inbox_monitors\")} · unpublished events {json_i64(&work, \"unpublished_events\")}"
            }
            if !sessions.is_empty() {
                h3 { class: "mt-4 mb-1 text-xs font-semibold uppercase tracking-wide text-[var(--app-muted)]",
                    "Active sign-ins"
                }
                ul { class: "m-0 list-none space-y-1 p-0",
                    for s in sessions.into_iter().take(20) {
                        {
                            let email = json_str(&s, "email").to_string();
                            let ua = json_str(&s, "user_agent");
                            let ip = json_str(&s, "ip");
                            let seen = s
                                .get("last_seen_at")
                                .and_then(|v| v.as_str())
                                .map(short_time)
                                .unwrap_or_else(|| "—".into());
                            rsx! {
                                li { class: "text-[11px] text-[var(--app-muted)]",
                                    "{email} · {ip} · {seen} · {ua}"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn CountBlock(title: String, value: Value) -> Element {
    let text = fmt_counts(&value);
    rsx! {
        div { class: "rounded-lg border border-[var(--app-border)] bg-[var(--app-bg)] px-3 py-2",
            p { class: "m-0 text-[11px] uppercase tracking-wide text-[var(--app-muted)]", "{title}" }
            p { class: "m-0 mt-1 text-xs text-[var(--app-fg)]", "{text}" }
        }
    }
}

#[component]
fn InstanceSection(instance: Value) -> Element {
    let health = instance.get("health").cloned().unwrap_or(Value::Null);
    let urls = instance.get("urls").cloned().unwrap_or(Value::Null);
    let providers = instance.get("providers").cloned().unwrap_or(Value::Null);
    let oauth = instance.get("oauth_configured").cloned().unwrap_or(Value::Null);
    let packages = instance
        .get("packages")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let host_ver = instance
        .get("package_versions")
        .and_then(|v| v.get("fleet-host"))
        .and_then(|v| v.as_str())
        .unwrap_or("—");
    let driver_ver = instance
        .get("package_versions")
        .and_then(|v| v.get("fleet-driver"))
        .and_then(|v| v.as_str())
        .unwrap_or("—");
    let signup = json_bool(&instance, "signup_token_required");
    let bootstrap = json_bool(&instance, "bootstrap_pair_enabled");
    rsx! {
        section { class: "mt-8 rounded-2xl border border-[var(--app-border)] bg-[var(--app-bg-elevated)] p-4",
            h2 { class: "m-0 text-sm font-semibold text-[var(--app-fg)]", "Control plane" }
            p { class: "mt-2 text-sm text-[var(--app-fg)]",
                "API {json_str(&health, \"status\")} · DB {json_str(&health, \"db\")} · git {json_str(&health, \"git_sha\")}"
            }
            p { class: "mt-1 text-xs text-[var(--app-muted)]",
                "API {json_str(&urls, \"api_public_base\")} · UI {json_str(&urls, \"ui_public_base\")} · workers {json_str(&urls, \"worker_public_base\")}"
            }
            p { class: "mt-1 text-xs text-[var(--app-muted)]",
                if signup { "Signup token required" } else { "Open signup" }
                " · bootstrap pair "
                if bootstrap { "on" } else { "off" }
                " · fleet-host {host_ver} · fleet-driver {driver_ver}"
            }
            p { class: "mt-2 text-xs text-[var(--app-muted)]",
                "LLM: ollama={json_bool(&providers, \"ollama\")} grok={json_bool(&providers, \"grok\")} openai={json_bool(&providers, \"openai\")} claude={json_bool(&providers, \"claude\")}"
            }
            p { class: "mt-1 text-xs text-[var(--app-muted)]",
                "OAuth configured: {fmt_true_keys(&oauth)}"
            }
            if packages.is_empty() {
                p { class: "mt-2 text-xs text-[var(--app-muted)]",
                    "No staged worker packages. Run scripts/stage-worker-dist.sh before remote auto-update."
                }
            } else {
                ul { class: "mt-2 m-0 list-none space-y-1 p-0",
                    for p in packages {
                        li { class: "text-xs text-[var(--app-muted)]",
                            "{json_str(&p, \"platform\")} · {json_str(&p, \"git_sha\")}"
                        }
                    }
                }
            }
        }
    }
}
