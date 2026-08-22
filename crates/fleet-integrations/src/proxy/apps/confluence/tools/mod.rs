//! Confluence Cloud allowlisted ops.

mod pages;

use serde_json::Value;

use crate::proxy::types::ProxyResult;

pub const OPERATIONS: &[&str] = &[
    "search",
    "list_spaces",
    "list_pages",
    "get_page",
    "create_page",
    "update_page",
    "add_comment",
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
            "confluence",
            operation,
            "misconfigured",
            "Confluence connection missing cloud_id — reconnect the integration",
            None,
        );
    }
    match operation {
        "search" => pages::search(http, token, cloud_id, args).await,
        "list_spaces" => pages::list_spaces(http, token, cloud_id, args).await,
        "list_pages" => pages::list_pages(http, token, cloud_id, args).await,
        "get_page" => pages::get_page(http, token, cloud_id, args).await,
        "create_page" => pages::create_page(http, token, cloud_id, args).await,
        "update_page" => pages::update_page(http, token, cloud_id, args).await,
        "add_comment" => pages::add_comment(http, token, cloud_id, args).await,
        other => ProxyResult::err(
            "confluence",
            other,
            "unknown_op",
            format!(
                "unknown confluence operation `{other}`; valid: {}",
                OPERATIONS.join(", ")
            ),
            None,
        ),
    }
}
