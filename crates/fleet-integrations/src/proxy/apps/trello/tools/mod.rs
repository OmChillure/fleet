//! Trello tools.

mod boards;

use serde_json::Value;

use crate::proxy::types::ProxyResult;

pub const OPERATIONS: &[&str] = &[
    "list_boards",
    "list_lists",
    "list_cards",
    "get_card",
    "create_card",
    "update_card",
    "add_comment",
];

pub async fn invoke(
    http: &reqwest::Client,
    token: &str,
    api_key: &str,
    operation: &str,
    args: &Value,
) -> ProxyResult {
    if api_key.trim().is_empty() {
        return super::client::missing_key(operation);
    }
    match operation {
        "list_boards" => boards::list_boards(http, token, api_key, args).await,
        "list_lists" => boards::list_lists(http, token, api_key, args).await,
        "list_cards" => boards::list_cards(http, token, api_key, args).await,
        "get_card" => boards::get_card(http, token, api_key, args).await,
        "create_card" => boards::create_card(http, token, api_key, args).await,
        "update_card" => boards::update_card(http, token, api_key, args).await,
        "add_comment" => boards::add_comment(http, token, api_key, args).await,
        other => ProxyResult::err(
            "trello",
            other,
            "unknown_op",
            format!(
                "unknown trello operation `{other}`; valid: {}",
                OPERATIONS.join(", ")
            ),
            None,
        ),
    }
}
