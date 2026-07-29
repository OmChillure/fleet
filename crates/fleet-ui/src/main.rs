//! Agent Fleet web UI — landing, auth, and app shell.

#![allow(non_snake_case)]

mod components;
mod pages;
mod theme;

use dioxus::prelude::*;

use components::app_shell::AppShell;
use components::navbar::Navbar;
use pages::agents::Agents;
use pages::dashboard::Dashboard;
use pages::home::Home;
use pages::auth::Login;
use pages::sandboxes::Sandboxes;
use pages::sessions::Sessions;
use pages::settings::Settings;

#[derive(Debug, Clone, Routable, PartialEq)]
#[rustfmt::skip]
pub enum Route {
    /// Marketing: no app sidebar / theme toggle.
    #[layout(MarketingLayout)]
        #[route("/")]
        Home {},
        #[route("/login")]
        Login {},
    #[end_layout]
    /// App: sidebar + black/white theme toggle (top right).
    #[layout(AppShell)]
        #[route("/dashboard")]
        Dashboard {},
        #[route("/sessions")]
        Sessions {},
        #[route("/agents")]
        Agents {},
        #[route("/sandboxes")]
        Sandboxes {},
        #[route("/settings")]
        Settings {},
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
