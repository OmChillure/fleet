//! Jira tools.

mod issues;

use serde_json::Value;

use crate::proxy::types::ProxyResult;

pub const OPERATIONS: &[&str] = &[
    "search_issues",
    "get_issue",
    "list_projects",
    "create_issue",
    "add_comment",
    "list_transitions",
    "transition_issue",
];

pub async fn invoke(
    http: &reqwest::Client,
    token: &str,
    cloud_id: &str,
    operation: &str,
    args: &Value,
) -> ProxyResult {
    if cloud_id.trim().is_empty() {
        return ProxyResult::err(
            "jira",
            operation,
            "misconfigured",
            "Jira connection missing cloud_id — reconnect the integration",
            None,
        );
    }
    match operation {
        "search_issues" => issues::search_issues(http, token, cloud_id, args).await,
        "get_issue" => issues::get_issue(http, token, cloud_id, args).await,
        "list_projects" => issues::list_projects(http, token, cloud_id, args).await,
        "create_issue" => issues::create_issue(http, token, cloud_id, args).await,
        "add_comment" => issues::add_comment(http, token, cloud_id, args).await,
        "list_transitions" => issues::list_transitions(http, token, cloud_id, args).await,
        "transition_issue" => issues::transition_issue(http, token, cloud_id, args).await,
        other => ProxyResult::err(
            "jira",
            other,
            "unknown_op",
            format!(
                "unknown jira operation `{other}`; valid: {}",
                OPERATIONS.join(", ")
            ),
            None,
        ),
    }
}
