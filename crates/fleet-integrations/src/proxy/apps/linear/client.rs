//! Linear HTTP / GraphQL helpers.

use serde_json::{Value, json};

use crate::proxy::types::ProxyResult;

pub(crate) fn map_issue(i: &Value) -> Value {
    let desc = i.get("description").and_then(|v| v.as_str()).unwrap_or("");
    let desc = if desc.chars().count() > 2000 {
        let t: String = desc.chars().take(2000).collect();
        format!("{t}…")
    } else {
        desc.to_string()
    };
    json!({
        "id": i.get("id"),
        "identifier": i.get("identifier"),
        "title": i.get("title"),
        "description": desc,
        "url": i.get("url"),
        "priority": i.get("priority"),
        "state": i.get("state").and_then(|s| s.get("name")),
        "state_type": i.get("state").and_then(|s| s.get("type")),
        "assignee": i.get("assignee").and_then(|a| a.get("name")),
        "team": i.get("team").and_then(|t| t.get("key")),
        "team_id": i.get("team").and_then(|t| t.get("id")),
        "created_at": i.get("createdAt"),
        "updated_at": i.get("updatedAt"),
    })
}

pub(crate) async fn graphql(
    http: &reqwest::Client,
    token: &str,
    query: &str,
    variables: Value,
) -> (u16, Value) {
    match http
        .post("https://api.linear.app/graphql")
        .header("Authorization", token)
        .header("Content-Type", "application/json")
        .json(&json!({ "query": query, "variables": variables }))
        .send()
        .await
    {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.json::<Value>().await.unwrap_or(json!({}));
            (status, body)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

pub(crate) fn upstream_err(op: &str, status: u16, body: &Value) -> ProxyResult {
    let msg = body
        .get("errors")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|e| e.get("message"))
        .and_then(|v| v.as_str())
        .or_else(|| body.get("error").and_then(|v| v.as_str()))
        .or_else(|| body.get("message").and_then(|v| v.as_str()))
        .unwrap_or("upstream error");
    let code = if status == 401 || status == 403 {
        "token_expired"
    } else {
        "upstream"
    };
    ProxyResult::err(
        "linear",
        op,
        code,
        format!("Linear API {status}: {msg}"),
        Some(status),
    )
}
