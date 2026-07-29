use dioxus::prelude::*;

use crate::pages::shared::EmptyPage;

#[component]
pub fn Settings() -> Element {
    rsx! {
        EmptyPage {
            title: "Settings",
            description: "Workspace, integrations, and preferences.",
        }
    }
}
