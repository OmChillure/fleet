//! Slack conversations + messaging tools.

use serde_json::{json, Value};

use crate::proxy::types::ProxyResult;
use super::super::client::*;

pub(crate) async fn list_channels(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
) -> ProxyResult {
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(50)
        .clamp(1, 200);
    let types = args
        .get("types")
        .and_then(|v| v.as_str())
        .unwrap_or("public_channel,private_channel");
    let url = format!(
        "https://slack.com/api/conversations.list?limit={limit}&types={types}&exclude_archived=true"
    );
    let (status, body) = slack_get(http, token, &url).await;
    if let Err(e) = slack_ok(status, &body, "list_channels") {
        return e;
    }
    let items: Vec<Value> = body
        .get("channels")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|c| {
                    json!({
                        "id": c.get("id"),
                        "name": c.get("name"),
                        "is_private": c.get("is_private"),
                        "is_im": c.get("is_im"),
                        "is_mpim": c.get("is_mpim"),
                        "num_members": c.get("num_members"),
                        "topic": c.get("topic").and_then(|t| t.get("value")),
                        "purpose": c.get("purpose").and_then(|t| t.get("value")),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "slack",
        "list_channels",
        format!("Listed {n} Slack conversations"),
        json!({ "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn list_users(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(50)
        .clamp(1, 200);
    let url = format!("https://slack.com/api/users.list?limit={limit}");
    let (status, body) = slack_get(http, token, &url).await;
    if let Err(e) = slack_ok(status, &body, "list_users") {
        return e;
    }
    let items: Vec<Value> = body
        .get("members")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|u| u.get("deleted").and_then(|d| d.as_bool()) != Some(true))
                .filter(|u| u.get("is_bot").and_then(|d| d.as_bool()) != Some(true))
                .map(|u| {
                    json!({
                        "id": u.get("id"),
                        "name": u.get("name"),
                        "real_name": u.get("real_name")
                            .or_else(|| u.get("profile").and_then(|p| p.get("real_name"))),
                        "display_name": u.get("profile").and_then(|p| p.get("display_name")),
                        "tz": u.get("tz"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "slack",
        "list_users",
        format!("Listed {n} Slack users"),
        json!({ "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn post_message(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
) -> ProxyResult {
    let channel = match req_str(args, "channel") {
        Ok(c) => c,
        Err(e) => return ProxyResult::err("slack", "post_message", "bad_args", e, None),
    };
    let channel = match resolve_channel_id(http, token, &channel).await {
        Ok(id) => id,
        Err(e) => return e,
    };
    let text = match args
        .get("text")
        .or_else(|| args.get("message"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(t) => t.to_string(),
        None => {
            return ProxyResult::err(
                "slack",
                "post_message",
                "bad_args",
                "text is required",
                None,
            );
        }
    };
    let mut payload = json!({ "channel": channel, "text": text });
    if let Some(ts) = args
        .get("thread_ts")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        payload["thread_ts"] = json!(ts);
    }
    let (status, body) = slack_post(http, token, "https://slack.com/api/chat.postMessage", &payload).await;
    if let Err(e) = slack_ok(status, &body, "post_message") {
        return e;
    }
    ProxyResult::ok_result(
        "slack",
        "post_message",
        format!("Posted Slack message to {channel}"),
        json!({
            "channel": body.get("channel").cloned().unwrap_or(json!(channel)),
            "ts": body.get("ts"),
            "message": body.get("message").map(|m| json!({
                "text": m.get("text"),
                "user": m.get("user"),
                "ts": m.get("ts"),
            })),
        }),
        Some(status),
    )
}

pub(crate) async fn get_channel_history(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
) -> ProxyResult {
    let channel = match req_str(args, "channel") {
        Ok(c) => c,
        Err(e) => {
            return ProxyResult::err("slack", "get_channel_history", "bad_args", e, None);
        }
    };
    let channel = match resolve_channel_id(http, token, &channel).await {
        Ok(id) => id,
        Err(e) => return e,
    };
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 100);
    let mut url = format!(
        "https://slack.com/api/conversations.history?channel={channel}&limit={limit}"
    );
    if let Some(oldest) = args.get("oldest").and_then(|v| v.as_str()) {
        url.push_str("&oldest=");
        url.push_str(oldest);
    }
    if let Some(latest) = args.get("latest").and_then(|v| v.as_str()) {
        url.push_str("&latest=");
        url.push_str(latest);
    }
    let (status, body) = slack_get(http, token, &url).await;
    if let Err(e) = slack_ok(status, &body, "get_channel_history") {
        return e;
    }
    let items: Vec<Value> = body
        .get("messages")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|m| {
                    json!({
                        "ts": m.get("ts"),
                        "user": m.get("user"),
                        "text": truncate_str(
                            m.get("text").and_then(|v| v.as_str()).unwrap_or(""),
                            1500,
                        ),
                        "thread_ts": m.get("thread_ts"),
                        "reply_count": m.get("reply_count"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "slack",
        "get_channel_history",
        format!("Fetched {n} Slack messages from {channel}"),
        json!({ "channel": channel, "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn get_thread(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let channel = match req_str(args, "channel") {
        Ok(c) => c,
        Err(e) => return ProxyResult::err("slack", "get_thread", "bad_args", e, None),
    };
    let channel = match resolve_channel_id(http, token, &channel).await {
        Ok(id) => id,
        Err(e) => return e,
    };
    let ts = match args
        .get("thread_ts")
        .or_else(|| args.get("ts"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(t) => t.to_string(),
        None => {
            return ProxyResult::err(
                "slack",
                "get_thread",
                "bad_args",
                "thread_ts is required",
                None,
            );
        }
    };
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 100);
    let url = format!(
        "https://slack.com/api/conversations.replies?channel={channel}&ts={ts}&limit={limit}"
    );
    let (status, body) = slack_get(http, token, &url).await;
    if let Err(e) = slack_ok(status, &body, "get_thread") {
        return e;
    }
    let items: Vec<Value> = body
        .get("messages")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|m| {
                    json!({
                        "ts": m.get("ts"),
                        "user": m.get("user"),
                        "text": truncate_str(
                            m.get("text").and_then(|v| v.as_str()).unwrap_or(""),
                            1500,
                        ),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "slack",
        "get_thread",
        format!("Fetched {n} Slack replies"),
        json!({ "channel": channel, "thread_ts": ts, "count": n, "items": items }),
        Some(status),
    )
}

/// Accept Slack channel id (`C…`) or a human name (`#eng` / `eng`).
pub(crate) fn match_listed_channel(items: &[Value], raw: &str) -> Option<String> {
    let want = raw.trim().trim_start_matches('#').to_ascii_lowercase();
    if want.is_empty() {
        return None;
    }
    for c in items {
        let name = c
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let id = c.get("id").and_then(|v| v.as_str()).unwrap_or("");
        if id.is_empty() {
            continue;
        }
        if name == want || id.eq_ignore_ascii_case(raw.trim()) {
            return Some(id.to_string());
        }
    }
    None
}

fn looks_like_slack_channel_id(raw: &str) -> bool {
    let s = raw.trim();
    s.len() >= 8
        && s.starts_with('C')
        && s.chars().all(|c| c.is_ascii_alphanumeric())
}

async fn resolve_channel_id(
    http: &reqwest::Client,
    token: &str,
    raw: &str,
) -> Result<String, ProxyResult> {
    let raw = raw.trim();
    if looks_like_slack_channel_id(raw) {
        return Ok(raw.to_string());
    }
    let listed = list_channels(http, token, &json!({ "limit": 200 })).await;
    if !listed.ok {
        return Err(listed);
    }
    let items = listed
        .data
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if let Some(id) = match_listed_channel(&items, raw) {
        return Ok(id);
    }
    Err(ProxyResult::err(
        "slack",
        "resolve_channel",
        "bad_args",
        format!("no Slack channel named `{raw}` — pass the channel id or connect a workspace that has it"),
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_hash_eng_name() {
        let items = vec![
            json!({"id": "C111", "name": "general"}),
            json!({"id": "CENG", "name": "eng"}),
        ];
        assert_eq!(match_listed_channel(&items, "#eng").as_deref(), Some("CENG"));
        assert_eq!(match_listed_channel(&items, "eng").as_deref(), Some("CENG"));
        assert_eq!(match_listed_channel(&items, "C111").as_deref(), Some("C111"));
        assert!(match_listed_channel(&items, "missing").is_none());
    }
}
