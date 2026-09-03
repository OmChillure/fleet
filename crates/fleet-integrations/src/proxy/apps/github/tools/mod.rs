//! Register new GitHub tools in OPERATIONS + match below.

mod reads;
mod writes;

use serde_json::Value;

use crate::proxy::types::ProxyResult;

pub const OPERATIONS: &[&str] = &[
    "list_repos",
    "list_issues",
    "get_issue",
    "list_prs",
    "get_pr",
    "get_file",
    "list_commits",
    "put_file",
    "commit_files",
    "create_branch",
    "create_pr",
    "create_issue",
    "update_issue",
    "add_comment",
    "merge_pr",
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
        "list_prs" => reads::list_prs(http, token, args).await,
        "get_pr" => reads::get_pr(http, token, args).await,
        "get_file" => reads::get_file(http, token, args).await,
        "list_commits" => reads::list_commits(http, token, args).await,
        "put_file" => writes::put_file(http, token, args).await,
        "commit_files" => writes::commit_files(http, token, args).await,
        "create_branch" => writes::create_branch(http, token, args).await,
        "create_pr" => writes::create_pr(http, token, args).await,
        "create_issue" => writes::create_issue(http, token, args).await,
        "update_issue" => writes::update_issue(http, token, args).await,
        "add_comment" => writes::add_comment(http, token, args).await,
        "merge_pr" => writes::merge_pr(http, token, args).await,
        other => ProxyResult::err(
            "github",
            other,
            "unknown_op",
            format!(
                "unknown github operation `{other}`; valid: {}",
                OPERATIONS.join(", ")
            ),
            None,
        ),
    }
}
