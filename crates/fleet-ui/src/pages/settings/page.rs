use dioxus::prelude::*;

use crate::pages::shared::EmptyPage;

#[component]
pub fn Settings(workspace_id: String) -> Element {
    let _ = &workspace_id;
    rsx! {
        EmptyPage {
            title: "Settings",
            description: "Workspace, integrations, and preferences.",
        }
    }
}
