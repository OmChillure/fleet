//! Register Teams / Graph tools in OPERATIONS + match below.

mod graph;

use serde_json::Value;

use crate::proxy::types::ProxyResult;

pub const OPERATIONS: &[&str] = &[
    "get_me",
    "list_chats",
    "list_teams",
    "list_channels",
    "list_chat_messages",
    "send_chat_message",
    "send_channel_message",
];

pub async fn invoke(
    http: &reqwest::Client,
    token: &str,
    operation: &str,
    args: &Value,
) -> ProxyResult {
    match operation {
        "get_me" => graph::get_me(http, token, args).await,
        "list_chats" => graph::list_chats(http, token, args).await,
        "list_teams" => graph::list_teams(http, token, args).await,
        "list_channels" => graph::list_channels(http, token, args).await,
        "list_chat_messages" => graph::list_chat_messages(http, token, args).await,
        "send_chat_message" => graph::send_chat_message(http, token, args).await,
        "send_channel_message" => graph::send_channel_message(http, token, args).await,
        other => ProxyResult::err(
            "teams",
            other,
            "unknown_op",
            format!(
                "unknown teams operation `{other}`; valid: {}",
                OPERATIONS.join(", ")
            ),
            None,
        ),
    }
}
