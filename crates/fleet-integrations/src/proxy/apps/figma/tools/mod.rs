//! Register Figma tools in OPERATIONS + match below.

mod files;

use serde_json::Value;

use crate::proxy::types::ProxyResult;

pub const OPERATIONS: &[&str] = &[
    "get_me",
    "get_file",
    "get_file_nodes",
    "list_comments",
    "list_team_projects",
    "get_project_files",
];

pub async fn invoke(
    http: &reqwest::Client,
    token: &str,
    operation: &str,
    args: &Value,
) -> ProxyResult {
    match operation {
        "get_me" => files::get_me(http, token, args).await,
        "get_file" => files::get_file(http, token, args).await,
        "get_file_nodes" => files::get_file_nodes(http, token, args).await,
        "list_comments" => files::list_comments(http, token, args).await,
        "list_team_projects" => files::list_team_projects(http, token, args).await,
        "get_project_files" => files::get_project_files(http, token, args).await,
        other => ProxyResult::err(
            "figma",
            other,
            "unknown_op",
            format!(
                "unknown figma operation `{other}`; valid: {}",
                OPERATIONS.join(", ")
            ),
            None,
        ),
    }
}
