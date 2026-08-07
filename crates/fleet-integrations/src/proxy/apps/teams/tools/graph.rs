//! Microsoft Graph Teams tools.

use serde_json::{json, Value};

use crate::proxy::types::ProxyResult;
use super::super::client::*;

pub(crate) async fn get_me(http: &reqwest::Client, token: &str, _args: &Value) -> ProxyResult {
    let (status, body) = graph_get(http, token, "https://graph.microsoft.com/v1.0/me").await;
    if !(200..300).contains(&status) {
        return upstream_err("get_me", status, &body);
    }
    ProxyResult::ok_result(
        "teams",
        "get_me",
        "Fetched Microsoft account",
        json!({
            "id": body.get("id"),
            "displayName": body.get("displayName"),
            "userPrincipalName": body.get("userPrincipalName"),
            "mail": body.get("mail"),
        }),
        Some(status),
    )
}

pub(crate) async fn list_chats(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let top = args
        .get("limit")
        .or_else(|| args.get("top"))
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50);
    let url = format!("https://graph.microsoft.com/v1.0/me/chats?$top={top}");
    let (status, body) = graph_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_chats", status, &body);
    }
    let items: Vec<Value> = body
        .get("value")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().map(map_chat).collect())
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "teams",
        "list_chats",
        format!("Listed {n} Teams chats"),
        json!({ "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn list_teams(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let top = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50);
    let url = format!("https://graph.microsoft.com/v1.0/me/joinedTeams?$top={top}");
    let (status, body) = graph_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_teams", status, &body);
    }
    let items: Vec<Value> = body
        .get("value")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().map(map_team).collect())
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "teams",
        "list_teams",
        format!("Listed {n} joined Teams"),
        json!({ "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn list_channels(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
) -> ProxyResult {
    let team_id = match req_str(args, "team_id") {
        Ok(t) => t,
        Err(e) => return ProxyResult::err("teams", "list_channels", "bad_args", e, None),
    };
    let url = format!("https://graph.microsoft.com/v1.0/teams/{team_id}/channels");
    let (status, body) = graph_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_channels", status, &body);
    }
    let items: Vec<Value> = body
        .get("value")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().map(map_channel).collect())
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "teams",
        "list_channels",
        format!("Listed {n} channels"),
        json!({ "team_id": team_id, "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn list_chat_messages(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
) -> ProxyResult {
    let chat_id = match req_str(args, "chat_id") {
        Ok(c) => c,
        Err(e) => {
            return ProxyResult::err("teams", "list_chat_messages", "bad_args", e, None);
        }
    };
    let top = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50);
    let url = format!("https://graph.microsoft.com/v1.0/chats/{chat_id}/messages?$top={top}");
    let (status, body) = graph_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_chat_messages", status, &body);
    }
    let items: Vec<Value> = body
        .get("value")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().map(map_message).collect())
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "teams",
        "list_chat_messages",
        format!("Fetched {n} chat messages"),
        json!({ "chat_id": chat_id, "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn send_chat_message(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
) -> ProxyResult {
    let chat_id = match req_str(args, "chat_id") {
        Ok(c) => c,
        Err(e) => {
            return ProxyResult::err("teams", "send_chat_message", "bad_args", e, None);
        }
    };
    let text = match args
        .get("text")
        .or_else(|| args.get("content"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(t) => t.to_string(),
        None => {
            return ProxyResult::err(
                "teams",
                "send_chat_message",
                "bad_args",
                "text is required",
                None,
            );
        }
    };
    let payload = json!({
        "body": { "contentType": "text", "content": text }
    });
    let url = format!("https://graph.microsoft.com/v1.0/chats/{chat_id}/messages");
    let (status, body) = graph_post(http, token, &url, &payload).await;
    if !(200..300).contains(&status) {
        return upstream_err("send_chat_message", status, &body);
    }
    ProxyResult::ok_result(
        "teams",
        "send_chat_message",
        "Sent Teams chat message",
        json!({
            "id": body.get("id"),
            "chat_id": chat_id,
            "webUrl": body.get("webUrl"),
        }),
        Some(status),
    )
}

pub(crate) async fn send_channel_message(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
) -> ProxyResult {
    let team_id = match req_str(args, "team_id") {
        Ok(t) => t,
        Err(e) => {
            return ProxyResult::err("teams", "send_channel_message", "bad_args", e, None);
        }
    };
    let channel_id = match req_str(args, "channel_id") {
        Ok(c) => c,
        Err(e) => {
            return ProxyResult::err("teams", "send_channel_message", "bad_args", e, None);
        }
    };
    let text = match args
        .get("text")
        .or_else(|| args.get("content"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(t) => t.to_string(),
        None => {
            return ProxyResult::err(
                "teams",
                "send_channel_message",
                "bad_args",
                "text is required",
                None,
            );
        }
    };
    let payload = json!({
        "body": { "contentType": "text", "content": text }
    });
    let url = format!(
        "https://graph.microsoft.com/v1.0/teams/{team_id}/channels/{channel_id}/messages"
    );
    let (status, body) = graph_post(http, token, &url, &payload).await;
    if !(200..300).contains(&status) {
        return upstream_err("send_channel_message", status, &body);
    }
    ProxyResult::ok_result(
        "teams",
        "send_channel_message",
        "Sent Teams channel message",
        json!({
            "id": body.get("id"),
            "team_id": team_id,
            "channel_id": channel_id,
            "webUrl": body.get("webUrl"),
        }),
        Some(status),
    )
}
