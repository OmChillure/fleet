//! Linear write + team tools.

use serde_json::{Value, json};

use super::super::client::*;
use crate::proxy::types::ProxyResult;

pub(crate) async fn list_teams(http: &reqwest::Client, token: &str, _args: &Value) -> ProxyResult {
    let query = r#"
      query {
        teams {
          nodes { id key name }
        }
      }
    "#;
    let (status, body) = graphql(http, token, query, json!({})).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_teams", status, &body);
    }
    if let Some(errs) = body.get("errors").and_then(|v| v.as_array())
        && !errs.is_empty()
    {
        let msg = errs[0]
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("GraphQL error");
        return ProxyResult::err(
            "linear",
            "list_teams",
            "upstream",
            format!("Linear GraphQL: {msg}"),
            Some(status),
        );
    }
    let items = body
        .pointer("/data/teams/nodes")
        .cloned()
        .unwrap_or(json!([]));
    let n = items.as_array().map(|a| a.len()).unwrap_or(0);
    ProxyResult::ok_result(
        "linear",
        "list_teams",
        format!("Listed {n} Linear teams"),
        json!({ "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn list_workflow_states(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
) -> ProxyResult {
    let team_id = match args
        .get("team_id")
        .or_else(|| args.get("teamId"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(id) => id.to_string(),
        None => {
            return ProxyResult::err(
                "linear",
                "list_workflow_states",
                "bad_args",
                "team_id is required",
                None,
            );
        }
    };
    let query = r#"
      query($teamId: String!) {
        team(id: $teamId) {
          states {
            nodes { id name type }
          }
        }
      }
    "#;
    let (status, body) = graphql(http, token, query, json!({ "teamId": team_id })).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_workflow_states", status, &body);
    }
    if let Some(errs) = body.get("errors").and_then(|v| v.as_array())
        && !errs.is_empty()
    {
        let msg = errs[0]
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("GraphQL error");
        return ProxyResult::err(
            "linear",
            "list_workflow_states",
            "upstream",
            format!("Linear GraphQL: {msg}"),
            Some(status),
        );
    }
    let items: Vec<Value> = body
        .pointer("/data/team/states/nodes")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|s| {
                    json!({
                        "id": s.get("id"),
                        "name": s.get("name"),
                        "type": s.get("type"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "linear",
        "list_workflow_states",
        format!("Listed {n} workflow states"),
        json!({ "team_id": team_id, "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn create_issue(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let title = match args
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(t) => t.to_string(),
        None => {
            return ProxyResult::err(
                "linear",
                "create_issue",
                "bad_args",
                "title is required",
                None,
            );
        }
    };
    let team_id = match args
        .get("team_id")
        .or_else(|| args.get("teamId"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(t) => t.to_string(),
        None => {
            return ProxyResult::err(
                "linear",
                "create_issue",
                "bad_args",
                "team_id is required (from list_teams)",
                None,
            );
        }
    };
    let mut input = json!({ "title": title, "teamId": team_id });
    if let Some(desc) = args.get("description").and_then(|v| v.as_str()) {
        input["description"] = json!(desc);
    }
    if let Some(p) = args.get("priority").and_then(|v| v.as_i64()) {
        input["priority"] = json!(p);
    }
    let query = r#"
      mutation($input: IssueCreateInput!) {
        issueCreate(input: $input) {
          success
          issue {
            id identifier title url priority
            state { name type }
            team { id key name }
          }
        }
      }
    "#;
    let (status, body) = graphql(http, token, query, json!({ "input": input })).await;
    if !(200..300).contains(&status) {
        return upstream_err("create_issue", status, &body);
    }
    if let Some(errs) = body.get("errors").and_then(|v| v.as_array())
        && !errs.is_empty()
    {
        let msg = errs[0]
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("GraphQL error");
        return ProxyResult::err(
            "linear",
            "create_issue",
            "upstream",
            format!("Linear GraphQL: {msg}"),
            Some(status),
        );
    }
    let created = body
        .pointer("/data/issueCreate/issue")
        .cloned()
        .unwrap_or(json!({}));
    let ident = created
        .get("identifier")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    ProxyResult::ok_result(
        "linear",
        "create_issue",
        format!("Created Linear issue {ident}"),
        map_issue(&created),
        Some(status),
    )
}

pub(crate) async fn update_issue(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let id = match args
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(i) => i.to_string(),
        None => {
            return ProxyResult::err("linear", "update_issue", "bad_args", "id is required", None);
        }
    };
    let mut input = json!({});
    if let Some(t) = args.get("title").and_then(|v| v.as_str()) {
        input["title"] = json!(t);
    }
    if let Some(d) = args.get("description").and_then(|v| v.as_str()) {
        input["description"] = json!(d);
    }
    if let Some(p) = args.get("priority").and_then(|v| v.as_i64()) {
        input["priority"] = json!(p);
    }
    if let Some(s) = args
        .get("state_id")
        .or_else(|| args.get("stateId"))
        .and_then(|v| v.as_str())
    {
        input["stateId"] = json!(s);
    }
    if input.as_object().map(|m| m.is_empty()).unwrap_or(true) {
        return ProxyResult::err(
            "linear",
            "update_issue",
            "bad_args",
            "provide title, description, priority, and/or state_id",
            None,
        );
    }
    let query = r#"
      mutation($id: String!, $input: IssueUpdateInput!) {
        issueUpdate(id: $id, input: $input) {
          success
          issue {
            id identifier title url priority description
            state { name type }
            assignee { name email }
            team { id key name }
            createdAt updatedAt
          }
        }
      }
    "#;
    let (status, body) = graphql(http, token, query, json!({ "id": id, "input": input })).await;
    if !(200..300).contains(&status) {
        return upstream_err("update_issue", status, &body);
    }
    if let Some(errs) = body.get("errors").and_then(|v| v.as_array())
        && !errs.is_empty()
    {
        let msg = errs[0]
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("GraphQL error");
        return ProxyResult::err(
            "linear",
            "update_issue",
            "upstream",
            format!("Linear GraphQL: {msg}"),
            Some(status),
        );
    }
    let issue = body
        .pointer("/data/issueUpdate/issue")
        .cloned()
        .unwrap_or(json!({}));
    ProxyResult::ok_result(
        "linear",
        "update_issue",
        "Updated Linear issue",
        map_issue(&issue),
        Some(status),
    )
}

pub(crate) async fn add_comment(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let issue_id = match args
        .get("issue_id")
        .or_else(|| args.get("id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(i) => i.to_string(),
        None => {
            return ProxyResult::err(
                "linear",
                "add_comment",
                "bad_args",
                "issue_id is required",
                None,
            );
        }
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
            return ProxyResult::err(
                "linear",
                "add_comment",
                "bad_args",
                "body is required",
                None,
            );
        }
    };
    let query = r#"
      mutation($input: CommentCreateInput!) {
        commentCreate(input: $input) {
          success
          comment { id body createdAt }
        }
      }
    "#;
    let (status, body) = graphql(
        http,
        token,
        query,
        json!({ "input": { "issueId": issue_id, "body": body_text } }),
    )
    .await;
    if !(200..300).contains(&status) {
        return upstream_err("add_comment", status, &body);
    }
    if let Some(errs) = body.get("errors").and_then(|v| v.as_array())
        && !errs.is_empty()
    {
        let msg = errs[0]
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("GraphQL error");
        return ProxyResult::err(
            "linear",
            "add_comment",
            "upstream",
            format!("Linear GraphQL: {msg}"),
            Some(status),
        );
    }
    ProxyResult::ok_result(
        "linear",
        "add_comment",
        "Added Linear comment",
        body.pointer("/data/commentCreate/comment")
            .cloned()
            .unwrap_or(json!({ "ok": true })),
        Some(status),
    )
}
