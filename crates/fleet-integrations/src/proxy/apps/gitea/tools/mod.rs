//! Register new Gitea tools in OPERATIONS + match below.

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
    "merge_mr",
];

pub async fn invoke(
    http: &reqwest::Client,
    token: &str,
    config: &Value,
    operation: &str,
    args: &Value,
) -> ProxyResult {
    let base = match super::client::base_url(config) {
        Ok(b) => b,
        Err(e) => {
            return ProxyResult::err("gitea", operation, "bad_config", e, None);
        }
    };
    match operation {
        "list_repos" => reads::list_repos(http, token, &base, args).await,
        "list_issues" => reads::list_issues(http, token, &base, args).await,
        "get_issue" => reads::get_issue(http, token, &base, args).await,
        "list_mrs" => reads::list_mrs(http, token, &base, args).await,
        "get_mr" => reads::get_mr(http, token, &base, args).await,
        "get_file" => reads::get_file(http, token, &base, args).await,
        "list_commits" => reads::list_commits(http, token, &base, args).await,
        "commit_files" => writes::commit_files(http, token, &base, args).await,
        "create_branch" => writes::create_branch(http, token, &base, args).await,
        "create_mr" => writes::create_mr(http, token, &base, args).await,
        "create_issue" => writes::create_issue(http, token, &base, args).await,
        "update_issue" => writes::update_issue(http, token, &base, args).await,
        "add_comment" => writes::add_comment(http, token, &base, args).await,
        "merge_mr" => writes::merge_mr(http, token, &base, args).await,
        other => ProxyResult::err(
            "gitea",
            other,
            "unknown_op",
            format!(
                "unknown gitea operation `{other}`; valid: {}",
                OPERATIONS.join(", ")
            ),
            None,
        ),
    }
}
