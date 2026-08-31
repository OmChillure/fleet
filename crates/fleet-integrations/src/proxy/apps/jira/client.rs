//! Jira HTTP helpers.

use serde_json::{Value, json};

use crate::proxy::types::ProxyResult;

pub(crate) fn req_str(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("{key} is required"))
}

pub(crate) fn adf_doc(text: &str) -> Value {
    json!({
        "type": "doc",
        "version": 1,
        "content": [{
            "type": "paragraph",
            "content": [{ "type": "text", "text": text.chars().take(8000).collect::<String>() }]
        }]
    })
}

pub(crate) fn map_issue(i: &Value) -> Value {
    let fields = i.get("fields").cloned().unwrap_or(json!({}));
    let status = fields
        .get("status")
        .and_then(|s| s.get("name"))
        .cloned()
        .unwrap_or(Value::Null);
    let project = fields
        .get("project")
        .and_then(|p| p.get("key"))
        .cloned()
        .unwrap_or(Value::Null);
    let assignee = fields
        .get("assignee")
        .and_then(|a| a.get("displayName").or_else(|| a.get("emailAddress")))
        .cloned()
        .unwrap_or(Value::Null);
    let issue_type = fields
        .get("issuetype")
        .and_then(|t| t.get("name"))
        .cloned()
        .unwrap_or(Value::Null);
    json!({
        "key": i.get("key"),
        "id": i.get("id"),
        "summary": fields.get("summary"),
        "status": status,
        "project": project,
        "assignee": assignee,
        "issuetype": issue_type,
        "priority": fields.get("priority").and_then(|p| p.get("name")),
        "updated": fields.get("updated"),
        "created": fields.get("created"),
        "self": i.get("self"),
    })
}

pub(crate) async fn jira_get(http: &reqwest::Client, token: &str, url: &str) -> (u16, Value) {
    match http
        .get(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
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

pub(crate) async fn jira_post(
    http: &reqwest::Client,
    token: &str,
    url: &str,
    body: &Value,
) -> (u16, Value) {
    match http
        .post(url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .json(body)
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
        .get("errorMessages")
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .or_else(|| body.get("message").and_then(|v| v.as_str()))
        .or_else(|| body.get("error").and_then(|v| v.as_str()))
        .unwrap_or("upstream error");
    let code = if status == 401 || status == 403 {
        "token_expired"
    } else {
        "upstream"
    };
    ProxyResult::err(
        "jira",
        op,
        code,
        format!("Jira API {status}: {msg}"),
        Some(status),
    )
}
