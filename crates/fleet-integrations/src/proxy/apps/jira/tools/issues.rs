//! Jira issue tools.

use serde_json::{Value, json};

use super::super::client::*;
use crate::proxy::types::ProxyResult;

pub(crate) async fn search_issues(
    http: &reqwest::Client,
    token: &str,
    cloud_id: &str,
    args: &Value,
) -> ProxyResult {
    let jql = args
        .get("jql")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            args.get("project")
                .and_then(|v| v.as_str())
                .map(|p| format!("project = {} ORDER BY updated DESC", p.trim()))
        })
        .unwrap_or_else(|| "ORDER BY updated DESC".into());

    let max = args
        .get("max_results")
        .and_then(|v| v.as_u64())
        .or_else(|| args.get("per_page").and_then(|v| v.as_u64()))
        .unwrap_or(20)
        .clamp(1, 50);

    // Prefer new search/jql endpoint; fall back to classic /search.
    let url = format!("https://api.atlassian.com/ex/jira/{cloud_id}/rest/api/3/search/jql");
    let payload = json!({
        "jql": jql,
        "maxResults": max,
        "fields": ["summary", "status", "assignee", "priority", "updated", "created", "issuetype", "project"]
    });
    let (status, body) = jira_post(http, token, &url, &payload).await;
    let (status, body) = if status == 404 || status == 410 {
        let url2 = format!("https://api.atlassian.com/ex/jira/{cloud_id}/rest/api/3/search");
        jira_post(
            http,
            token,
            &url2,
            &json!({
                "jql": jql,
                "maxResults": max,
                "fields": ["summary", "status", "assignee", "priority", "updated", "created", "issuetype", "project"]
            }),
        )
        .await
    } else {
        (status, body)
    };

    if !(200..300).contains(&status) {
        return upstream_err("search_issues", status, &body);
    }

    let issues = body
        .get("issues")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let items: Vec<Value> = issues.iter().map(map_issue).collect();
    let n = items.len();
    ProxyResult::ok_result(
        "jira",
        "search_issues",
        format!("Found {n} Jira issues"),
        json!({ "count": n, "jql": jql, "items": items }),
        Some(status),
    )
}

pub(crate) async fn get_issue(
    http: &reqwest::Client,
    token: &str,
    cloud_id: &str,
    args: &Value,
) -> ProxyResult {
    let key = args
        .get("key")
        .or_else(|| args.get("issue_key"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(key) = key else {
        return ProxyResult::err(
            "jira",
            "get_issue",
            "bad_args",
            "key is required (e.g. PROJ-123)",
            None,
        );
    };
    let url = format!(
        "https://api.atlassian.com/ex/jira/{cloud_id}/rest/api/3/issue/{key}?fields=summary,status,assignee,priority,updated,created,issuetype,project,description"
    );
    let (status, body) = jira_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("get_issue", status, &body);
    }
    ProxyResult::ok_result(
        "jira",
        "get_issue",
        format!("Fetched Jira issue {key}"),
        map_issue(&body),
        Some(status),
    )
}

pub(crate) async fn list_projects(
    http: &reqwest::Client,
    token: &str,
    cloud_id: &str,
    args: &Value,
) -> ProxyResult {
    let max = args
        .get("max_results")
        .and_then(|v| v.as_u64())
        .unwrap_or(50)
        .clamp(1, 100);
    let url = format!(
        "https://api.atlassian.com/ex/jira/{cloud_id}/rest/api/3/project/search?maxResults={max}"
    );
    let (status, body) = jira_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_projects", status, &body);
    }
    let items: Vec<Value> = body
        .get("values")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|p| {
                    json!({
                        "id": p.get("id"),
                        "key": p.get("key"),
                        "name": p.get("name"),
                        "projectTypeKey": p.get("projectTypeKey"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "jira",
        "list_projects",
        format!("Listed {n} Jira projects"),
        json!({ "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn create_issue(
    http: &reqwest::Client,
    token: &str,
    cloud_id: &str,
    args: &Value,
) -> ProxyResult {
    let project = match req_str(args, "project").or_else(|_| req_str(args, "project_key")) {
        Ok(p) => p,
        Err(e) => return ProxyResult::err("jira", "create_issue", "bad_args", e, None),
    };
    let summary = match req_str(args, "summary").or_else(|_| req_str(args, "title")) {
        Ok(s) => s,
        Err(e) => return ProxyResult::err("jira", "create_issue", "bad_args", e, None),
    };
    let issue_type = args
        .get("issuetype")
        .or_else(|| args.get("issue_type"))
        .and_then(|v| v.as_str())
        .unwrap_or("Task");
    let mut fields = json!({
        "project": { "key": project },
        "summary": summary,
        "issuetype": { "name": issue_type },
    });
    if let Some(desc) = args
        .get("description")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        fields["description"] = adf_doc(desc);
    }
    let url = format!("https://api.atlassian.com/ex/jira/{cloud_id}/rest/api/3/issue");
    let (status, body) = jira_post(http, token, &url, &json!({ "fields": fields })).await;
    if !(200..300).contains(&status) {
        return upstream_err("create_issue", status, &body);
    }
    let key = body.get("key").and_then(|v| v.as_str()).unwrap_or("");
    ProxyResult::ok_result(
        "jira",
        "create_issue",
        format!("Created Jira issue {key}"),
        json!({
            "id": body.get("id"),
            "key": body.get("key"),
            "self": body.get("self"),
        }),
        Some(status),
    )
}

pub(crate) async fn add_comment(
    http: &reqwest::Client,
    token: &str,
    cloud_id: &str,
    args: &Value,
) -> ProxyResult {
    let key = match req_str(args, "key").or_else(|_| req_str(args, "issue_key")) {
        Ok(k) => k,
        Err(e) => return ProxyResult::err("jira", "add_comment", "bad_args", e, None),
    };
    let body_text = match args
        .get("body")
        .or_else(|| args.get("comment"))
        .or_else(|| args.get("text"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(t) => t.to_string(),
        None => {
            return ProxyResult::err("jira", "add_comment", "bad_args", "body is required", None);
        }
    };
    let url =
        format!("https://api.atlassian.com/ex/jira/{cloud_id}/rest/api/3/issue/{key}/comment");
    let (status, body) =
        jira_post(http, token, &url, &json!({ "body": adf_doc(&body_text) })).await;
    if !(200..300).contains(&status) {
        return upstream_err("add_comment", status, &body);
    }
    ProxyResult::ok_result(
        "jira",
        "add_comment",
        format!("Commented on Jira {key}"),
        json!({ "id": body.get("id"), "key": key, "created": body.get("created") }),
        Some(status),
    )
}

pub(crate) async fn list_transitions(
    http: &reqwest::Client,
    token: &str,
    cloud_id: &str,
    args: &Value,
) -> ProxyResult {
    let key = match req_str(args, "key").or_else(|_| req_str(args, "issue_key")) {
        Ok(k) => k,
        Err(e) => return ProxyResult::err("jira", "list_transitions", "bad_args", e, None),
    };
    let url =
        format!("https://api.atlassian.com/ex/jira/{cloud_id}/rest/api/3/issue/{key}/transitions");
    let (status, body) = jira_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_transitions", status, &body);
    }
    let items: Vec<Value> = body
        .get("transitions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|t| {
                    json!({
                        "id": t.get("id"),
                        "name": t.get("name"),
                        "to": t.get("to").and_then(|s| s.get("name")),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "jira",
        "list_transitions",
        format!("Listed {n} transitions for {key}"),
        json!({ "key": key, "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn transition_issue(
    http: &reqwest::Client,
    token: &str,
    cloud_id: &str,
    args: &Value,
) -> ProxyResult {
    let key = match req_str(args, "key").or_else(|_| req_str(args, "issue_key")) {
        Ok(k) => k,
        Err(e) => return ProxyResult::err("jira", "transition_issue", "bad_args", e, None),
    };
    let transition_id = match args
        .get("transition_id")
        .or_else(|| args.get("id"))
        .and_then(|v| {
            v.as_str()
                .map(|s| s.to_string())
                .or_else(|| v.as_u64().map(|n| n.to_string()))
        })
        .filter(|s| !s.is_empty())
    {
        Some(id) => id,
        None => {
            return ProxyResult::err(
                "jira",
                "transition_issue",
                "bad_args",
                "transition_id is required (from list_transitions)",
                None,
            );
        }
    };
    let url =
        format!("https://api.atlassian.com/ex/jira/{cloud_id}/rest/api/3/issue/{key}/transitions");
    let (status, body) = jira_post(
        http,
        token,
        &url,
        &json!({ "transition": { "id": transition_id } }),
    )
    .await;
    if !(200..300).contains(&status) {
        return upstream_err("transition_issue", status, &body);
    }
    ProxyResult::ok_result(
        "jira",
        "transition_issue",
        format!("Transitioned Jira {key}"),
        json!({ "key": key, "transition_id": transition_id }),
        Some(status),
    )
}
