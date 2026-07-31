use dioxus::prelude::*;

use crate::pages::shared::EmptyPage;

#[component]
pub fn Sessions(workspace_id: String) -> Element {
    let _ = &workspace_id;
    rsx! {
        EmptyPage {
            title: "Sessions",
            description: "Agent sessions and live streams will appear here.",
        }
    }
}
