use dioxus::prelude::*;

use crate::pages::shared::EmptyPage;

#[component]
pub fn Agents() -> Element {
    rsx! {
        EmptyPage {
            title: "Agents",
            description: "Configure agent roles, models, and budgets.",
        }
    }
}
