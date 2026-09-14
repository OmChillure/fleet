//! One view per inbox app. Shared chrome lives in `shared`; layout lives here.

use dioxus::prelude::*;

mod asana;
mod bitbucket;
mod confluence;
mod github;
mod gitlab;
mod jira;
mod linear;
mod notion;
mod slack;
mod teams;
mod trello;

use super::catalog::InboxAppInfo;
use super::shared::PageData;

pub fn render(ws: &str, info: &'static InboxAppInfo, data: PageData) -> Element {
    match info.id {
        "github" => github::view(ws, data),
        "gitlab" => gitlab::view(ws, data),
        "bitbucket" => bitbucket::view(ws, data),
        "jira" => jira::view(ws, data),
        "linear" => linear::view(ws, data),
        "trello" => trello::view(ws, data),
        "asana" => asana::view(ws, data),
        "slack" => slack::view(ws, data),
        "teams" => teams::view(ws, data),
        "notion" => notion::view(ws, data),
        "confluence" => confluence::view(ws, data),
        _ => super::shared::frame(rsx! {
            p { class: "text-sm text-[var(--app-muted)]", "Unknown inbox app." }
        }),
    }
}
