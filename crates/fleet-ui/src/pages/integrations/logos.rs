//! Integration brand logos (SVG assets under `assets/integrations/`).
//!
//! Files downloaded from Simple Icons (CC0): see `assets/integrations/README.md`.

use dioxus::prelude::*;

/// Brand tile + logo image (or monogram fallback).
#[component]
pub fn IntegrationLogo(icon: String, name: String) -> Element {
    let monogram = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into());

    let (tile_class, logo) = logo_for(&icon);

    rsx! {
        div {
            class: "flex h-10 w-10 shrink-0 items-center justify-center rounded-xl {tile_class}",
            if let Some(src) = logo {
                img {
                    class: if icon == "gitea" { "h-10 w-10 object-contain" } else { "h-5 w-5 object-contain" },
                    src: "{src}",
                    alt: "{name}",
                    width: "20",
                    height: "20",
                }
            } else {
                span { class: "text-sm font-bold text-white", "{monogram}" }
            }
        }
    }
}

/// Returns (tile background classes, optional asset URL).
fn logo_for(icon: &str) -> (&'static str, Option<Asset>) {
    match icon {
        "github" => (
            "bg-[#181717]",
            Some(asset!("/assets/integrations/github.svg")),
        ),
        "jira" => (
            "bg-[#0052CC]",
            Some(asset!("/assets/integrations/jira.svg")),
        ),
        "slack" => (
            "bg-[#4A154B]",
            Some(asset!("/assets/integrations/slack.svg")),
        ),
        "linear" => (
            "bg-[#5E6AD2]",
            Some(asset!("/assets/integrations/linear.svg")),
        ),
        "gitlab" => (
            "bg-[#FC6D26]",
            Some(asset!("/assets/integrations/gitlab.svg")),
        ),
        "gitea" => (
            "bg-transparent",
            Some(asset!("/assets/integrations/gitea.svg")),
        ),
        "bitbucket" => (
            "bg-[#0052CC]",
            Some(asset!("/assets/integrations/bitbucket.svg")),
        ),
        "teams" => (
            "bg-[#6264A7]",
            Some(asset!("/assets/integrations/teams.svg")),
        ),
        "discord" => (
            "bg-[#5865F2]",
            Some(asset!("/assets/integrations/discord.svg")),
        ),
        "figma" => (
            "bg-[#1E1E1E] ring-1 ring-white/10",
            Some(asset!("/assets/integrations/figma.svg")),
        ),
        "notion" => (
            "bg-[#000000] ring-1 ring-white/10",
            Some(asset!("/assets/integrations/notion.svg")),
        ),
        "confluence" => (
            "bg-[#172B4D]",
            Some(asset!("/assets/integrations/confluence.svg")),
        ),
        "asana" => (
            "bg-[#F06A6A]",
            Some(asset!("/assets/integrations/asana.svg")),
        ),
        "trello" => (
            "bg-[#0052CC]",
            Some(asset!("/assets/integrations/trello.svg")),
        ),
        "google_drive" => (
            "bg-[#1a73e8]",
            Some(asset!("/assets/integrations/google_drive.svg")),
        ),
        _ => ("bg-neutral-700", None),
    }
}
