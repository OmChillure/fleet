//! Per-provider apps. Add: `apps/<name>/{client,tools}` + register here + OAuth.

pub mod asana;
pub mod bitbucket;
pub mod confluence;
pub mod discord;
pub mod figma;
pub mod github;
pub mod gitlab;
pub mod google_drive;
pub mod jira;
pub mod linear;
pub mod notion;
pub mod slack;
pub mod teams;
pub mod trello;

use serde_json::Value;

use crate::proxy::types::ProxyResult;

pub fn supported_providers() -> &'static [&'static str] {
    &[
        "github",
        "gitlab",
        "jira",
        "linear",
        "slack",
        "figma",
        "notion",
        "google_drive",
        "teams",
        "discord",
        "trello",
        "asana",
        "bitbucket",
        "confluence",
    ]
}

/// Full allowlist as lines the model can read without calling `get_integration`.
pub fn operations_catalog_text() -> String {
    supported_providers()
        .iter()
        .map(|p| format!("{p}: {}", operations_for(p).join(", ")))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn operations_for(provider: &str) -> &'static [&'static str] {
    match provider {
        "github" => github::OPERATIONS,
        "gitlab" => gitlab::OPERATIONS,
        "jira" => jira::OPERATIONS,
        "linear" => linear::OPERATIONS,
        "slack" => slack::OPERATIONS,
        "figma" => figma::OPERATIONS,
        "notion" => notion::OPERATIONS,
        "google_drive" => google_drive::OPERATIONS,
        "teams" => teams::OPERATIONS,
        "discord" => discord::OPERATIONS,
        "trello" => trello::OPERATIONS,
        "asana" => asana::OPERATIONS,
        "bitbucket" => bitbucket::OPERATIONS,
        "confluence" => confluence::OPERATIONS,
        _ => &[],
    }
}

pub async fn dispatch(
    provider: &str,
    http: &reqwest::Client,
    access_token: &str,
    operation: &str,
    args: &Value,
    config: &Value,
) -> ProxyResult {
    match provider {
        "github" => github::invoke(http, access_token, operation, args).await,
        "gitlab" => gitlab::invoke(http, access_token, operation, args).await,
        "jira" => {
            let cloud_id = config
                .get("cloud_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            jira::invoke(http, access_token, cloud_id, operation, args).await
        }
        "linear" => linear::invoke(http, access_token, operation, args).await,
        "slack" => slack::invoke(http, access_token, operation, args).await,
        "figma" => figma::invoke(http, access_token, operation, args).await,
        "notion" => notion::invoke(http, access_token, operation, args).await,
        "google_drive" => google_drive::invoke(http, access_token, operation, args).await,
        "teams" => teams::invoke(http, access_token, operation, args).await,
        "discord" => discord::invoke(http, access_token, operation, args).await,
        "trello" => {
            let api_key = config.get("api_key").and_then(|v| v.as_str()).unwrap_or("");
            trello::invoke(http, access_token, api_key, operation, args).await
        }
        "asana" => asana::invoke(http, access_token, operation, args).await,
        "bitbucket" => bitbucket::invoke(http, access_token, operation, args).await,
        "confluence" => {
            let cloud_id = config
                .get("cloud_id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            confluence::invoke(http, access_token, cloud_id, operation, args).await
        }
        _ => ProxyResult::err(provider, operation, "unknown_provider", "unsupported", None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn supported_providers_has_unique_ids() {
        let ids = supported_providers();
        assert_eq!(ids.len(), 14);
        let unique: HashSet<_> = ids.iter().copied().collect();
        assert_eq!(unique.len(), 14);
        assert!(ids.contains(&"gitlab"));
        assert!(ids.contains(&"trello"));
        assert!(ids.contains(&"asana"));
        assert!(ids.contains(&"bitbucket"));
        assert!(ids.contains(&"confluence"));
    }

    #[test]
    fn bitbucket_and_confluence_ops_registered() {
        assert!(operations_for("bitbucket").contains(&"create_pr"));
        assert!(operations_for("confluence").contains(&"create_page"));
    }

    #[test]
    fn github_operations_include_commit_files_and_create_pr() {
        let ops = operations_for("github");
        assert!(ops.contains(&"commit_files"));
        assert!(ops.contains(&"create_pr"));
    }

    #[test]
    fn slack_and_notion_have_test_trio_ops() {
        let slack = operations_for("slack");
        assert!(slack.contains(&"list_channels"));
        assert!(slack.contains(&"post_message"));
        assert!(slack.contains(&"get_thread"));
        let notion = operations_for("notion");
        assert!(notion.contains(&"search"));
        assert!(notion.contains(&"get_block_children"));
        assert!(notion.contains(&"create_page"));
        assert!(notion.contains(&"append_blocks"));
    }

    #[test]
    fn unknown_provider_has_no_operations() {
        assert!(operations_for("unknown").is_empty());
    }

    #[test]
    fn operations_catalog_lists_every_provider() {
        let text = operations_catalog_text();
        for p in supported_providers() {
            assert!(text.contains(&format!("{p}:")), "missing {p} in {text}");
        }
        assert!(text.contains("post_message"));
        assert!(text.contains("commit_files"));
    }
}
