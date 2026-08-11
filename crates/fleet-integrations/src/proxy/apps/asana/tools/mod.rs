//! Asana tools.

mod tasks;

use serde_json::Value;

use crate::proxy::types::ProxyResult;

pub const OPERATIONS: &[&str] = &[
    "list_workspaces",
    "list_projects",
    "list_tasks",
    "get_task",
    "create_task",
    "update_task",
    "add_comment",
];

pub async fn invoke(
    http: &reqwest::Client,
    token: &str,
    operation: &str,
    args: &Value,
) -> ProxyResult {
    match operation {
        "list_workspaces" => tasks::list_workspaces(http, token, args).await,
        "list_projects" => tasks::list_projects(http, token, args).await,
        "list_tasks" => tasks::list_tasks(http, token, args).await,
        "get_task" => tasks::get_task(http, token, args).await,
        "create_task" => tasks::create_task(http, token, args).await,
        "update_task" => tasks::update_task(http, token, args).await,
        "add_comment" => tasks::add_comment(http, token, args).await,
        other => ProxyResult::err(
            "asana",
            other,
            "unknown_op",
            format!(
                "unknown asana operation `{other}`; valid: {}",
                OPERATIONS.join(", ")
            ),
            None,
        ),
    }
}
