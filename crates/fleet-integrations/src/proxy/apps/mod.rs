//! Per-provider apps. Add: `apps/<name>/{client,tools}` + register here + OAuth.

pub mod asana;
pub mod bitbucket;
pub mod confluence;
pub mod discord;
pub mod figma;
pub mod gitea;
pub mod github;
pub mod gogs;
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
        "gitea",
        "gogs",
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
        "gitea" => gitea::OPERATIONS,
        "gogs" => gogs::OPERATIONS,
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
        "github" => github::invoke(http, access_token, operation, args, config).await,
        "gitlab" => gitlab::invoke(http, access_token, operation, args).await,
        "gitea" => gitea::invoke(http, access_token, config, operation, args).await,
        "gogs" => gogs::invoke(http, access_token, config, operation, args).await,
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
        let mut seen = HashSet::new();
        for &id in ids {
            assert!(
                seen.insert(id),
                "duplicate provider id `{id}` in supported_providers: {ids:?}"
            );
        }
    }

    #[test]
    fn supported_providers_is_the_published_allowlist() {
        assert_eq!(
            supported_providers(),
            &[
                "github",
                "gitlab",
                "gitea",
                "gogs",
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
        );
    }

    #[test]
    fn every_supported_provider_has_nonempty_operations_for() {
        for p in supported_providers() {
            assert!(
                !operations_for(p).is_empty(),
                "supported provider `{p}` must have a non-empty operations_for arm"
            );
        }
    }

    #[tokio::test]
    async fn invoke_unknown_provider_is_rejected_before_db() {
        let pool = fleet_db::PgPool::connect_lazy("postgres://unused:unused@127.0.0.1:9/none")
            .expect("lazy pool");
        pool.close().await;
        let key = crate::CredentialsKey::from_secret("dev-test-encryption-key-32chars!!")
            .expect("test key");
        let got = crate::proxy::invoke(
            &pool,
            &key,
            crate::proxy::ProxyRequest {
                workspace_id: uuid::Uuid::new_v4(),
                provider: "notaprovider".into(),
                operation: "list_repos".into(),
                args: serde_json::json!({}),
                caller: crate::proxy::ProxyCaller::System,
            },
        )
        .await;
        assert!(!got.ok, "unknown provider must be ok:false: {got:?}");
        assert_eq!(
            got.error_code.as_deref(),
            Some("unknown_provider"),
            "notaprovider must be unknown_provider and must not hit find_connected (closed pool would be internal): {got:?}"
        );
    }

    #[test]
    fn slack_operations_for_is_the_published_five() {
        assert_eq!(
            operations_for("slack"),
            &[
                "list_channels",
                "list_users",
                "post_message",
                "get_channel_history",
                "get_thread",
            ]
        );
    }

    #[test]
    fn notion_operations_for_is_the_published_seven() {
        assert_eq!(
            operations_for("notion"),
            &[
                "search",
                "get_page",
                "get_database",
                "query_database",
                "get_block_children",
                "create_page",
                "append_blocks",
            ]
        );
    }

    #[tokio::test]
    async fn invoke_unknown_slack_or_notion_op_is_unknown_op() {
        let http = reqwest::Client::new();
        let args = serde_json::json!({});
        let slack = slack::invoke(&http, "token", "not_an_op", &args).await;
        assert!(!slack.ok, "unknown slack op must be ok:false: {slack:?}");
        assert_eq!(
            slack.error_code.as_deref(),
            Some("unknown_op"),
            "slack::invoke(\"not_an_op\") must be unknown_op (match arm, not OPERATIONS-only): {slack:?}"
        );
        let notion = notion::invoke(&http, "token", "not_an_op", &args).await;
        assert!(!notion.ok, "unknown notion op must be ok:false: {notion:?}");
        assert_eq!(
            notion.error_code.as_deref(),
            Some("unknown_op"),
            "notion::invoke(\"not_an_op\") must be unknown_op (match arm, not OPERATIONS-only): {notion:?}"
        );
    }
}
