//! Agent Fleet web UI — landing, auth, and app shell.

#![allow(non_snake_case)]

mod clipboard;
mod components;
mod pages;
mod theme;

use dioxus::prelude::*;

use components::app_shell::AppShell;
use components::navbar::Navbar;
use components::toast::ToastProvider;
use fleet_runners_view::routes::{
    SEGMENT_CLOUD, SEGMENT_PARTNERS, runner_child_path, runners_index_target, self_hosted_path,
    workers_redirect,
};
use pages::admin::Admin;
use pages::agents::Agents;
use pages::auth::{Login, Signup};
use pages::cases::{CaseDetail, Cases};
use pages::chat::{Chat, ChatThread};
use pages::dashboard::Dashboard;
use pages::fleet::Fleet;
use pages::home::Home;
use pages::inbox::Inbox;
use pages::integrations::{Integrations, OAuthDone};
use pages::runners::cloud::CloudPage;
use pages::runners::partners::PartnersPage;
use pages::sandboxes::Sandboxes;
use pages::sessions::{SessionDetail, Sessions};
use pages::settings::Settings;
use pages::workers::Workers as SelfHosted;

#[derive(Debug, Clone, Routable, PartialEq)]
#[rustfmt::skip]
pub enum Route {
    // Marketing: no app sidebar / theme toggle.
    #[layout(MarketingLayout)]
        #[route("/")]
        Home {},
        #[route("/login")]
        Login {},
        #[route("/signup")]
        Signup {},
    #[end_layout]
    // OAuth popup only — no shell, no navbar. Closes after signaling parent.
    #[route("/oauth/done")]
    OAuthDone {},
    // App shell: `/{workspace_id}/dashboard` etc.
    #[layout(AppShell)]
        #[route("/admin")]
        Admin {},
        #[route("/:workspace_id/dashboard")]
        Dashboard { workspace_id: String },
        #[route("/:workspace_id/chat")]
        Chat { workspace_id: String },
        #[route("/:workspace_id/chat/:conversation_id")]
        ChatThread { workspace_id: String, conversation_id: String },
        #[route("/:workspace_id/fleet")]
        Fleet { workspace_id: String },
        #[route("/:workspace_id/sessions/:session_id")]
        SessionDetail { workspace_id: String, session_id: String },
        #[route("/:workspace_id/sessions")]
        Sessions { workspace_id: String },
        #[route("/:workspace_id/agents")]
        Agents { workspace_id: String },
        #[route("/:workspace_id/sandboxes")]
        Sandboxes { workspace_id: String },
        #[route("/:workspace_id/cases/:case_id")]
        CaseDetail { workspace_id: String, case_id: String },
        #[route("/:workspace_id/cases")]
        Cases { workspace_id: String },
        #[route("/:workspace_id/inbox")]
        Inbox { workspace_id: String },
        #[route("/:workspace_id/integrations")]
        Integrations { workspace_id: String },
        #[route("/:workspace_id/runners/self-hosted")]
        SelfHosted { workspace_id: String },
        #[route("/:workspace_id/runners/cloud")]
        CloudPage { workspace_id: String },
        #[route("/:workspace_id/runners/partners")]
        PartnersPage { workspace_id: String },
        #[route("/:workspace_id/runners")]
        RunnersIndex { workspace_id: String },
        #[route("/:workspace_id/workers")]
        WorkersRedirect { workspace_id: String },
        #[route("/:workspace_id/settings")]
        Settings { workspace_id: String },
}

impl Route {
    /// Active workspace id when inside the app shell routes.
    pub fn workspace_id(&self) -> Option<&str> {
        match self {
            Route::Dashboard { workspace_id }
            | Route::Chat { workspace_id }
            | Route::ChatThread { workspace_id, .. }
            | Route::Fleet { workspace_id }
            | Route::Sessions { workspace_id }
            | Route::SessionDetail { workspace_id, .. }
            | Route::Agents { workspace_id }
            | Route::Sandboxes { workspace_id }
            | Route::Cases { workspace_id }
            | Route::CaseDetail { workspace_id, .. }
            | Route::Inbox { workspace_id }
            | Route::Integrations { workspace_id }
            | Route::SelfHosted { workspace_id }
            | Route::CloudPage { workspace_id }
            | Route::PartnersPage { workspace_id }
            | Route::RunnersIndex { workspace_id }
            | Route::WorkersRedirect { workspace_id }
            | Route::Settings { workspace_id } => Some(workspace_id.as_str()),
            _ => None,
        }
    }
}

fn main() {
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    rsx! {
        document::Stylesheet { href: asset!("/assets/tailwind.css") }
        document::Link { rel: "icon", href: asset!("/assets/app-icon.png") }
        document::Link { rel: "apple-touch-icon", href: asset!("/assets/app-icon.png") }
        ToastProvider {
            Router::<Route> {}
        }
    }
}

/// Map a host-tested runners path onto the typed Dioxus route.
fn route_for_runners_path(workspace_id: String, path: &str) -> Route {
    if path == self_hosted_path(&workspace_id) {
        Route::SelfHosted { workspace_id }
    } else if path == runner_child_path(&workspace_id, SEGMENT_CLOUD) {
        Route::CloudPage { workspace_id }
    } else if path == runner_child_path(&workspace_id, SEGMENT_PARTNERS) {
        Route::PartnersPage { workspace_id }
    } else {
        Route::SelfHosted { workspace_id }
    }
}

/// `/{ws}/runners` → current self-hosted workers UI.
#[component]
fn RunnersIndex(workspace_id: String) -> Element {
    let navigator = use_navigator();
    let dest = runners_index_target(&workspace_id);
    let route = route_for_runners_path(workspace_id.clone(), &dest);
    use_hook(move || {
        navigator.replace(route.clone());
    });
    // Paint Self-hosted immediately so a missed replace is not a blank main.
    rsx! { SelfHosted { workspace_id } }
}

/// `/{ws}/workers` → `/{ws}/runners/self-hosted`.
#[component]
fn WorkersRedirect(workspace_id: String) -> Element {
    let navigator = use_navigator();
    let dest = workers_redirect(&workspace_id);
    let route = route_for_runners_path(workspace_id.clone(), &dest);
    use_hook(move || {
        navigator.replace(route.clone());
    });
    rsx! { SelfHosted { workspace_id } }
}

/// Landing + auth chrome — pure black, no color accents.
#[component]
fn MarketingLayout() -> Element {
    rsx! {
        div { class: "flex min-h-screen flex-col bg-black text-white",
            Navbar {}
            main { class: "w-full flex-1 bg-black",
                Outlet::<Route> {}
            }
            footer { class: "border-t border-neutral-800 bg-black px-4 py-4 text-center text-sm text-neutral-500",
                "Agent Fleet · orchestrate sandboxed coding agents"
            }
        }
    }
}
