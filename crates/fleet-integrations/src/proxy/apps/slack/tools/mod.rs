//! Register Slack tools in OPERATIONS + match below.

mod conversations;

use serde_json::Value;

use crate::proxy::types::ProxyResult;

pub const OPERATIONS: &[&str] = &[
    "list_channels",
    "list_users",
    "post_message",
    "get_channel_history",
];

pub async fn invoke(
    http: &reqwest::Client,
    token: &str,
    operation: &str,
    args: &Value,
) -> ProxyResult {
    match operation {
        "list_channels" => conversations::list_channels(http, token, args).await,
        "list_users" => conversations::list_users(http, token, args).await,
        "post_message" => conversations::post_message(http, token, args).await,
        "get_channel_history" => conversations::get_channel_history(http, token, args).await,
        other => ProxyResult::err(
            "slack",
            other,
            "unknown_op",
            format!(
                "unknown slack operation `{other}`; valid: {}",
                OPERATIONS.join(", ")
            ),
            None,
        ),
    }
}
