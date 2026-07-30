use dioxus::prelude::*;

use crate::pages::shared::EmptyPage;

#[component]
pub fn Sandboxes(workspace_id: String) -> Element {
    let _ = &workspace_id;
    rsx! {
        EmptyPage {
            title: "Sandboxes",
            description: "Sandbox health, backends, and resources.",
        }
    }
}
