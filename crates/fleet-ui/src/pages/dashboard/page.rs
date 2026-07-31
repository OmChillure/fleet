use dioxus::prelude::*;

use crate::pages::shared::EmptyPage;

#[component]
pub fn Dashboard(workspace_id: String) -> Element {
    let _ = &workspace_id;
    // Later: load data via `super::api::…` scoped to workspace_id.
    rsx! {
        EmptyPage {
            title: "Dashboard",
            description: "Fleet overview will live here.",
        }
    }
}
