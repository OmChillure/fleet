//! Discord identify + guilds (user OAuth; no bot posting).

use serde_json::{json, Value};

use crate::proxy::types::ProxyResult;
use super::super::client::*;

pub(crate) async fn get_me(http: &reqwest::Client, token: &str, _args: &Value) -> ProxyResult {
    let (status, body) = discord_get(http, token, "https://discord.com/api/v10/users/@me").await;
    if !(200..300).contains(&status) {
        return upstream_err("get_me", status, &body);
    }
    ProxyResult::ok_result(
        "discord",
        "get_me",
        "Fetched Discord user",
        json!({
            "id": body.get("id"),
            "username": body.get("username"),
            "global_name": body.get("global_name"),
            "discriminator": body.get("discriminator"),
        }),
        Some(status),
    )
}

pub(crate) async fn list_guilds(
    http: &reqwest::Client,
    token: &str,
    _args: &Value,
) -> ProxyResult {
    let (status, body) =
        discord_get(http, token, "https://discord.com/api/v10/users/@me/guilds").await;
    if !(200..300).contains(&status) {
        return upstream_err("list_guilds", status, &body);
    }
    let items: Vec<Value> = body
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|g| {
                    json!({
                        "id": g.get("id"),
                        "name": g.get("name"),
                        "owner": g.get("owner"),
                        "permissions": g.get("permissions"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "discord",
        "list_guilds",
        format!("Listed {n} Discord guilds"),
        json!({
            "count": n,
            "items": items,
            "note": "User OAuth cannot post messages; reconnect with a bot app for channel writes.",
        }),
        Some(status),
    )
}
