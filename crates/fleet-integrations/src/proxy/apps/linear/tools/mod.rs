//! Linear tools.

mod issues;
mod writes;

use serde_json::Value;

use crate::proxy::types::ProxyResult;

pub const OPERATIONS: &[&str] = &[
    "list_issues",
    "get_issue",
    "list_teams",
    "create_issue",
    "update_issue",
    "add_comment",
];

pub async fn invoke(
    http: &reqwest::Client,
    token: &str,
    operation: &str,
    args: &Value,
) -> ProxyResult {
    match operation {
        "list_issues" => issues::list_issues(http, token, args).await,
        "get_issue" => issues::get_issue(http, token, args).await,
        "list_teams" => writes::list_teams(http, token, args).await,
        "create_issue" => writes::create_issue(http, token, args).await,
        "update_issue" => writes::update_issue(http, token, args).await,
        "add_comment" => writes::add_comment(http, token, args).await,
        other => ProxyResult::err(
            "linear",
            other,
            "unknown_op",
            format!("unknown linear operation `{other}`; valid: {}", OPERATIONS.join(", ")),
            None,
        ),
    }
}
