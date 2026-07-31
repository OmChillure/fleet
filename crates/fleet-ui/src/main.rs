//! Agent Fleet web UI — landing, auth, and app shell.

#![allow(non_snake_case)]

mod components;
mod pages;
mod theme;

use dioxus::prelude::*;

use components::app_shell::AppShell;
use components::navbar::Navbar;
use pages::agents::Agents;
use pages::auth::{Login, Signup};
use pages::dashboard::Dashboard;
use pages::home::Home;
use pages::sandboxes::Sandboxes;
use pages::sessions::Sessions;
use pages::settings::Settings;

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
    // App shell: `/{workspace_id}/dashboard` etc.
    // `end_layout` must hang on the next variant (not trail the enum).
    #[end_layout]
    #[layout(AppShell)]
        #[route("/:workspace_id/dashboard")]
        Dashboard { workspace_id: String },
        #[route("/:workspace_id/sessions")]
        Sessions { workspace_id: String },
        #[route("/:workspace_id/agents")]
        Agents { workspace_id: String },
        #[route("/:workspace_id/sandboxes")]
        Sandboxes { workspace_id: String },
        #[route("/:workspace_id/settings")]
        Settings { workspace_id: String },
}

impl Route {
    /// Active workspace id when inside the app shell routes.
    pub fn workspace_id(&self) -> Option<&str> {
        match self {
            Route::Dashboard { workspace_id }
            | Route::Sessions { workspace_id }
            | Route::Agents { workspace_id }
            | Route::Sandboxes { workspace_id }
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
        Router::<Route> {}
    }
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
