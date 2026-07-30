use dioxus::prelude::*;

use crate::pages::shared::EmptyPage;

#[component]
pub fn Agents(workspace_id: String) -> Element {
    let _ = &workspace_id;
    rsx! {
        EmptyPage {
            title: "Agents",
            description: "Configure agent roles, models, and budgets.",
        }
    }
}
