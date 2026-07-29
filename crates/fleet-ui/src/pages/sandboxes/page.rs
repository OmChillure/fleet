use dioxus::prelude::*;

use crate::pages::shared::EmptyPage;

#[component]
pub fn Sandboxes() -> Element {
    rsx! {
        EmptyPage {
            title: "Sandboxes",
            description: "Sandbox health, backends, and resources.",
        }
    }
}
