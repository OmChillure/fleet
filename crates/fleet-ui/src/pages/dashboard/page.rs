use dioxus::prelude::*;

use crate::pages::shared::EmptyPage;

#[component]
pub fn Dashboard() -> Element {
    // Later: load data via `super::api::…` and render real widgets here.
    rsx! {
        EmptyPage {
            title: "Dashboard",
            description: "Fleet overview will live here.",
        }
    }
}
