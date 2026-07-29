use dioxus::prelude::*;

use crate::pages::shared::EmptyPage;

#[component]
pub fn Sessions() -> Element {
    rsx! {
        EmptyPage {
            title: "Sessions",
            description: "Agent sessions and live streams will appear here.",
        }
    }
}
