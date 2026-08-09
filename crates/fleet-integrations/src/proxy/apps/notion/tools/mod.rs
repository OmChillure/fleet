//! Register Notion tools in OPERATIONS + match below.

mod pages;

use serde_json::Value;

use crate::proxy::types::ProxyResult;

pub const OPERATIONS: &[&str] = &[
    "search",
    "get_page",
    "get_database",
    "query_database",
    "get_block_children",
    "create_page",
    "append_blocks",
];

pub async fn invoke(
    http: &reqwest::Client,
    token: &str,
    operation: &str,
    args: &Value,
) -> ProxyResult {
    match operation {
        "search" => pages::search(http, token, args).await,
        "get_page" => pages::get_page(http, token, args).await,
        "get_database" => pages::get_database(http, token, args).await,
        "query_database" => pages::query_database(http, token, args).await,
        "get_block_children" => pages::get_block_children(http, token, args).await,
        "create_page" => pages::create_page(http, token, args).await,
        "append_blocks" => pages::append_blocks(http, token, args).await,
        other => ProxyResult::err(
            "notion",
            other,
            "unknown_op",
            format!(
                "unknown notion operation `{other}`; valid: {}",
                OPERATIONS.join(", ")
            ),
            None,
        ),
    }
}
