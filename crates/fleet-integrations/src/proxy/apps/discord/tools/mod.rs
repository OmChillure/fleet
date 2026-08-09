//! Register Discord tools in OPERATIONS + match below.

mod me;

use serde_json::Value;

use crate::proxy::types::ProxyResult;

pub const OPERATIONS: &[&str] = &["get_me", "list_guilds"];

pub async fn invoke(
    http: &reqwest::Client,
    token: &str,
    operation: &str,
    args: &Value,
) -> ProxyResult {
    match operation {
        "get_me" => me::get_me(http, token, args).await,
        "list_guilds" => me::list_guilds(http, token, args).await,
        other => ProxyResult::err(
            "discord",
            other,
            "unknown_op",
            format!(
                "unknown discord operation `{other}`; valid: {}",
                OPERATIONS.join(", ")
            ),
            None,
        ),
    }
}
