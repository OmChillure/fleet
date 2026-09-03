//! Trello board / list / card tools.

use serde_json::{Value, json};

use super::super::client::*;
use crate::proxy::types::ProxyResult;

pub(crate) async fn list_boards(
    http: &reqwest::Client,
    token: &str,
    api_key: &str,
    args: &Value,
) -> ProxyResult {
    let filter = args
        .get("filter")
        .and_then(|v| v.as_str())
        .unwrap_or("open");
    let (status, body) = trello_get(
        http,
        token,
        api_key,
        &format!("/members/me/boards?filter={filter}&fields=name,url,closed,idOrganization"),
    )
    .await;
    if !(200..300).contains(&status) {
        return upstream_err("list_boards", status, &body);
    }
    let items: Vec<Value> = body
        .as_array()
        .map(|arr| arr.iter().map(map_board).collect())
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "trello",
        "list_boards",
        format!("Listed {n} Trello boards"),
        json!({ "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn list_lists(
    http: &reqwest::Client,
    token: &str,
    api_key: &str,
    args: &Value,
) -> ProxyResult {
    let board_id = match req_str(args, "board_id").or_else(|_| req_str(args, "idBoard")) {
        Ok(id) => id,
        Err(e) => return ProxyResult::err("trello", "list_lists", "bad_args", e, None),
    };
    let (status, body) = trello_get(
        http,
        token,
        api_key,
        &format!("/boards/{board_id}/lists?filter=open"),
    )
    .await;
    if !(200..300).contains(&status) {
        return upstream_err("list_lists", status, &body);
    }
    let items: Vec<Value> = body
        .as_array()
        .map(|arr| arr.iter().map(map_list).collect())
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "trello",
        "list_lists",
        format!("Listed {n} lists on board {board_id}"),
        json!({ "board_id": board_id, "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn list_cards(
    http: &reqwest::Client,
    token: &str,
    api_key: &str,
    args: &Value,
) -> ProxyResult {
    let list_id = args
        .get("list_id")
        .or_else(|| args.get("idList"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let board_id = args
        .get("board_id")
        .or_else(|| args.get("idBoard"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let path = if let Some(list_id) = list_id {
        format!("/lists/{list_id}/cards")
    } else if let Some(board_id) = board_id {
        format!("/boards/{board_id}/cards")
    } else {
        return ProxyResult::err(
            "trello",
            "list_cards",
            "bad_args",
            "list_id or board_id is required",
            None,
        );
    };
    let (status, body) = trello_get(http, token, api_key, &path).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_cards", status, &body);
    }
    let items: Vec<Value> = body
        .as_array()
        .map(|arr| arr.iter().map(map_card).collect())
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "trello",
        "list_cards",
        format!("Listed {n} Trello cards"),
        json!({ "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn get_card(
    http: &reqwest::Client,
    token: &str,
    api_key: &str,
    args: &Value,
) -> ProxyResult {
    let id = match req_str(args, "card_id").or_else(|_| req_str(args, "id")) {
        Ok(id) => id,
        Err(e) => return ProxyResult::err("trello", "get_card", "bad_args", e, None),
    };
    let (status, body) = trello_get(http, token, api_key, &format!("/cards/{id}")).await;
    if !(200..300).contains(&status) {
        return upstream_err("get_card", status, &body);
    }
    ProxyResult::ok_result(
        "trello",
        "get_card",
        "Fetched Trello card",
        map_card(&body),
        Some(status),
    )
}

pub(crate) async fn create_card(
    http: &reqwest::Client,
    token: &str,
    api_key: &str,
    args: &Value,
) -> ProxyResult {
    let list_id = match req_str(args, "list_id").or_else(|_| req_str(args, "idList")) {
        Ok(id) => id,
        Err(e) => return ProxyResult::err("trello", "create_card", "bad_args", e, None),
    };
    let name = match req_str(args, "name").or_else(|_| req_str(args, "title")) {
        Ok(n) => n,
        Err(e) => return ProxyResult::err("trello", "create_card", "bad_args", e, None),
    };
    let mut payload = json!({ "idList": list_id, "name": name });
    if let Some(desc) = args
        .get("desc")
        .or_else(|| args.get("description"))
        .or_else(|| args.get("body"))
        .and_then(|v| v.as_str())
    {
        payload["desc"] = json!(desc);
    }
    let (status, body) = trello_post(http, token, api_key, "/cards", &payload).await;
    if !(200..300).contains(&status) {
        return upstream_err("create_card", status, &body);
    }
    ProxyResult::ok_result(
        "trello",
        "create_card",
        format!("Created Trello card `{name}`"),
        map_card(&body),
        Some(status),
    )
}

pub(crate) async fn update_card(
    http: &reqwest::Client,
    token: &str,
    api_key: &str,
    args: &Value,
) -> ProxyResult {
    let id = match req_str(args, "card_id").or_else(|_| req_str(args, "id")) {
        Ok(id) => id,
        Err(e) => return ProxyResult::err("trello", "update_card", "bad_args", e, None),
    };
    let mut payload = json!({});
    if let Some(n) = args
        .get("name")
        .or_else(|| args.get("title"))
        .and_then(|v| v.as_str())
    {
        payload["name"] = json!(n);
    }
    if let Some(d) = args
        .get("desc")
        .or_else(|| args.get("description"))
        .and_then(|v| v.as_str())
    {
        payload["desc"] = json!(d);
    }
    if let Some(list_id) = args
        .get("list_id")
        .or_else(|| args.get("idList"))
        .and_then(|v| v.as_str())
    {
        payload["idList"] = json!(list_id);
    }
    if let Some(closed) = args.get("closed").and_then(|v| v.as_bool()) {
        payload["closed"] = json!(closed);
    }
    if payload.as_object().map(|m| m.is_empty()).unwrap_or(true) {
        return ProxyResult::err(
            "trello",
            "update_card",
            "bad_args",
            "provide name, desc, list_id, and/or closed",
            None,
        );
    }
    let (status, body) = trello_put(http, token, api_key, &format!("/cards/{id}"), &payload).await;
    if !(200..300).contains(&status) {
        return upstream_err("update_card", status, &body);
    }
    ProxyResult::ok_result(
        "trello",
        "update_card",
        "Updated Trello card",
        map_card(&body),
        Some(status),
    )
}

pub(crate) async fn add_comment(
    http: &reqwest::Client,
    token: &str,
    api_key: &str,
    args: &Value,
) -> ProxyResult {
    let id = match req_str(args, "card_id").or_else(|_| req_str(args, "id")) {
        Ok(id) => id,
        Err(e) => return ProxyResult::err("trello", "add_comment", "bad_args", e, None),
    };
    let text = match args
        .get("text")
        .or_else(|| args.get("body"))
        .or_else(|| args.get("comment"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(t) => t.to_string(),
        None => {
            return ProxyResult::err(
                "trello",
                "add_comment",
                "bad_args",
                "text is required",
                None,
            );
        }
    };
    let (status, body) = trello_post(
        http,
        token,
        api_key,
        &format!("/cards/{id}/actions/comments"),
        &json!({ "text": text }),
    )
    .await;
    if !(200..300).contains(&status) {
        return upstream_err("add_comment", status, &body);
    }
    ProxyResult::ok_result(
        "trello",
        "add_comment",
        "Added Trello comment",
        json!({
            "id": body.get("id"),
            "card_id": id,
            "text": text,
        }),
        Some(status),
    )
}
