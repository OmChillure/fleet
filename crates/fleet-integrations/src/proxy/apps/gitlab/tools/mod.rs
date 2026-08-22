//! Register new GitLab tools in OPERATIONS + match below.

mod reads;
mod writes;

use serde_json::Value;

use crate::proxy::types::ProxyResult;

pub const OPERATIONS: &[&str] = &[
    "list_repos",
    "list_issues",
    "get_issue",
    "list_mrs",
    "get_mr",
    "get_file",
    "list_commits",
    "commit_files",
    "create_branch",
    "create_mr",
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
        "list_repos" => reads::list_repos(http, token, args).await,
        "list_issues" => reads::list_issues(http, token, args).await,
        "get_issue" => reads::get_issue(http, token, args).await,
        "list_mrs" => reads::list_mrs(http, token, args).await,
        "get_mr" => reads::get_mr(http, token, args).await,
        "get_file" => reads::get_file(http, token, args).await,
        "list_commits" => reads::list_commits(http, token, args).await,
        "commit_files" => writes::commit_files(http, token, args).await,
        "create_branch" => writes::create_branch(http, token, args).await,
        "create_mr" => writes::create_mr(http, token, args).await,
        "create_issue" => writes::create_issue(http, token, args).await,
        "update_issue" => writes::update_issue(http, token, args).await,
        "add_comment" => writes::add_comment(http, token, args).await,
        other => ProxyResult::err(
            "gitlab",
            other,
            "unknown_op",
            format!(
                "unknown gitlab operation `{other}`; valid: {}",
                OPERATIONS.join(", ")
            ),
            None,
        ),
    }
}
