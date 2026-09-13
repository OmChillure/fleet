//! Slack conversations + messaging tools.

use serde_json::{json, Value};

use super::super::client::*;
use crate::proxy::types::ProxyResult;

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
    let origin = slack_api_origin();
    let url = format!(
        "{origin}/conversations.list?limit={limit}&types={types}&exclude_archived=true"
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
    let origin = slack_api_origin();
    let url = format!("{origin}/users.list?limit={limit}");
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

pub(crate) async fn post_message(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
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
    let (status, body) = slack_post(
        http,
        token,
        "https://slack.com/api/chat.postMessage",
        &payload,
    )
    .await;
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
    let mut url =
        format!("https://slack.com/api/conversations.history?channel={channel}&limit={limit}");
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
    s.len() >= 8 && s.starts_with('C') && s.chars().all(|c| c.is_ascii_alphanumeric())
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
        format!(
            "no Slack channel named `{raw}` — pass the channel id or connect a workspace that has it"
        ),
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed_channels() -> Vec<Value> {
        vec![
            json!({"id": "C111", "name": "general"}),
            json!({"id": "CENG", "name": "eng"}),
        ]
    }

    #[test]
    fn matches_hash_eng_name() {
        let items = listed_channels();
        assert_eq!(
            match_listed_channel(&items, "#eng").as_deref(),
            Some("CENG")
        );
    }

    #[test]
    fn match_listed_channel_bare_eng_name() {
        let items = listed_channels();
        assert_eq!(match_listed_channel(&items, "eng").as_deref(), Some("CENG"));
    }

    #[test]
    fn match_listed_channel_listed_id() {
        let items = listed_channels();
        assert_eq!(
            match_listed_channel(&items, "C111").as_deref(),
            Some("C111")
        );
    }

    #[test]
    fn match_listed_channel_unknown_name_is_none() {
        let items = listed_channels();
        assert!(match_listed_channel(&items, "missing").is_none());
    }

    #[test]
    fn match_listed_channel_name_is_case_insensitive() {
        let items = listed_channels();
        assert_eq!(
            match_listed_channel(&items, "#Eng").as_deref(),
            Some("CENG")
        );
        assert_eq!(match_listed_channel(&items, "ENG").as_deref(), Some("CENG"));
    }

    #[test]
    fn match_listed_channel_trims_and_rejects_hash_only() {
        let items = listed_channels();
        assert_eq!(
            match_listed_channel(&items, " #eng ").as_deref(),
            Some("CENG")
        );
        assert!(match_listed_channel(&items, "#").is_none());
        assert!(match_listed_channel(&items, "").is_none());
        assert!(match_listed_channel(&items, "   ").is_none());
    }

    #[test]
    fn looks_like_slack_channel_id_c_prefix_len_ge_8() {
        assert!(looks_like_slack_channel_id("C01234567"));
        assert!(!looks_like_slack_channel_id("C111"));
        assert!(!looks_like_slack_channel_id("#C01234567"));
        assert!(!looks_like_slack_channel_id("c01234567"));
    }
}

#[cfg(test)]
mod list_ops_sink_tests {
    use super::*;
    use crate::proxy::dispatch;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    struct OriginGuard;
    impl Drop for OriginGuard {
        fn drop(&mut self) {
            TEST_SLACK_API_BASE.with(|c| *c.borrow_mut() = None);
        }
    }

    struct SlackSink {
        hits: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        origin: String,
    }

    impl Drop for SlackSink {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
        }
    }

    impl SlackSink {
        fn bind(body: Value) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind slack sink");
            listener.set_nonblocking(true).expect("nonblocking sink");
            let addr = listener.local_addr().expect("sink addr");
            let hits = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let hits_t = hits.clone();
            let stop_t = stop.clone();
            let payload = body.to_string();
            thread::spawn(move || {
                while !stop_t.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            hits_t.fetch_add(1, Ordering::SeqCst);
                            let _ = stream.set_nonblocking(false);
                            let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                            let mut buf = Vec::new();
                            let mut tmp = [0u8; 4096];
                            loop {
                                match stream.read(&mut tmp) {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        buf.extend_from_slice(&tmp[..n]);
                                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                            break;
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                            let resp = format!(
                                "HTTP/1.1 200 ok\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                                payload.len()
                            );
                            let _ = stream.write_all(resp.as_bytes());
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                hits,
                stop,
                origin: format!("http://{addr}"),
            }
        }

        fn hits(&self) -> usize {
            self.hits.load(Ordering::SeqCst)
        }
    }

    async fn dispatch_slack(origin: &str, token: &str, operation: &str) -> ProxyResult {
        let _guard = OriginGuard;
        TEST_SLACK_API_BASE.with(|c| *c.borrow_mut() = Some(origin.to_string()));
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("http");
        dispatch("slack", &http, token, operation, &json!({}), &json!({})).await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn slack_list_channels_local_sink_maps_id_name_and_count() {
        const LEAK: &str = "xoxb-LIST-CHANNELS-MUST-NOT-ECHO-c0ffee";
        let sink = SlackSink::bind(json!({
            "ok": true,
            "access_token": LEAK,
            "channels": [{
                "id": "C0ENGTEST",
                "name": "eng",
                "is_private": false,
                "topic": { "value": "engineering" }
            }]
        }));
        let result = dispatch_slack(&sink.origin, LEAK, "list_channels").await;
        assert!(result.ok, "conversations.list 200 must be ok: {result:?}");
        assert_eq!(
            result.data["count"],
            json!(1),
            "count must match items: {result:?}"
        );
        assert_eq!(
            result.data["items"][0]["id"],
            json!("C0ENGTEST"),
            "must map channels[].id: {result:?}"
        );
        assert_eq!(
            result.data["items"][0]["name"],
            json!("eng"),
            "must map channels[].name: {result:?}"
        );
        let dump = serde_json::to_string(&result).expect("serialize");
        assert!(
            !dump.contains(LEAK) && !dump.to_ascii_lowercase().contains("bearer"),
            "list_channels must not echo bearer/token: {dump}"
        );
        assert!(
            sink.hits() > 0,
            "list_channels must hit the sink: {result:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn slack_list_users_local_sink_drops_deleted_and_bots() {
        const LEAK: &str = "xoxb-LIST-USERS-MUST-NOT-ECHO-0b0d";
        let sink = SlackSink::bind(json!({
            "ok": true,
            "access_token": LEAK,
            "members": [
                {
                    "id": "U0DELETED",
                    "name": "gone",
                    "deleted": true,
                    "is_bot": false,
                    "real_name": "Gone"
                },
                {
                    "id": "U0BOTUSER",
                    "name": "hubot",
                    "deleted": false,
                    "is_bot": true,
                    "real_name": "Hubot"
                },
                {
                    "id": "U0HUMAN01",
                    "name": "ada",
                    "deleted": false,
                    "is_bot": false,
                    "real_name": "Ada"
                }
            ]
        }));
        let result = dispatch_slack(&sink.origin, LEAK, "list_users").await;
        assert!(result.ok, "users.list 200 must be ok: {result:?}");
        let items = result.data["items"]
            .as_array()
            .unwrap_or_else(|| panic!("items: {result:?}"));
        assert_eq!(
            result.data["count"],
            json!(1),
            "only the human remains: {result:?}"
        );
        assert_eq!(items.len(), 1, "deleted+bot must be dropped: {result:?}");
        assert_eq!(items[0]["id"], json!("U0HUMAN01"), "{result:?}");
        assert_eq!(items[0]["name"], json!("ada"), "{result:?}");
        assert!(
            items.iter().all(|u| {
                u["id"].as_str() != Some("U0DELETED") && u["id"].as_str() != Some("U0BOTUSER")
            }),
            "must drop deleted and bots: {result:?}"
        );
        let dump = serde_json::to_string(&result).expect("serialize");
        assert!(!dump.contains(LEAK), "list_users must not echo token: {dump}");
        assert!(sink.hits() > 0, "list_users must hit the sink: {result:?}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn slack_invalid_auth_is_token_expired() {
        const LEAK: &str = "xoxb-INVALID-AUTH-MUST-NOT-ECHO-dead";
        let sink = SlackSink::bind(json!({
            "ok": false,
            "error": "invalid_auth",
            "access_token": LEAK
        }));
        let result = dispatch_slack(&sink.origin, LEAK, "list_channels").await;
        assert!(!result.ok, "invalid_auth must be ok:false: {result:?}");
        assert_eq!(
            result.error_code.as_deref(),
            Some("token_expired"),
            "Slack invalid_auth must map to token_expired: {result:?}"
        );
        let dump = serde_json::to_string(&result).expect("serialize");
        assert!(
            !dump.contains(LEAK),
            "token_expired must not echo token: {dump}"
        );
        assert!(sink.hits() > 0, "invalid_auth must hit the sink: {result:?}");
    }
}
