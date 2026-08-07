//! Linear issue tools.

use serde_json::{json, Value};

use crate::proxy::types::ProxyResult;
use super::super::client::*;

pub(crate) async fn list_issues(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let first = args
        .get("limit")
        .or_else(|| args.get("per_page"))
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50);
    let query = r#"
      query($first: Int!) {
        issues(first: $first) {
          nodes {
            id
            identifier
            title
            description
            url
            priority
            createdAt
            updatedAt
            state { name type }
            assignee { name email }
            team { key name }
          }
        }
      }
    "#;
    let (status, body) = graphql(http, token, query, json!({ "first": first })).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_issues", status, &body);
    }
    if let Some(errs) = body.get("errors").and_then(|v| v.as_array())
        && !errs.is_empty() {
            let msg = errs[0]
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("GraphQL error");
            return ProxyResult::err(
                "linear",
                "list_issues",
                "upstream",
                format!("Linear GraphQL: {msg}"),
                Some(status),
            );
        }
    let nodes = body
        .pointer("/data/issues/nodes")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let items: Vec<Value> = nodes.iter().map(map_issue).collect();
    let n = items.len();
    ProxyResult::ok_result(
        "linear",
        "list_issues",
        format!("Listed {n} Linear issues"),
        json!({ "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn get_issue(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
) -> ProxyResult {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let identifier = args
        .get("identifier")
        .or_else(|| args.get("key"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let Some(lookup) = id.or(identifier) else {
        return ProxyResult::err(
            "linear",
            "get_issue",
            "bad_args",
            "id or identifier required (e.g. ENG-123)",
            None,
        );
    };

    // Prefer direct id query; otherwise search by term (identifier).
    let (status, body) = if args.get("id").and_then(|v| v.as_str()).is_some() {
        let q = r#"
          query($id: String!) {
            issue(id: $id) {
              id identifier title description url priority createdAt updatedAt
              state { name type }
              assignee { name email }
              team { key name }
            }
          }
        "#;
        graphql(http, token, q, json!({ "id": lookup })).await
    } else {
        let q = r#"
          query($term: String!) {
            searchIssues(term: $term, first: 5) {
              nodes {
                id identifier title description url priority createdAt updatedAt
                state { name type }
                assignee { name email }
                team { key name }
              }
            }
          }
        "#;
        let (st, body) = graphql(http, token, q, json!({ "term": lookup })).await;
        if (200..300).contains(&st)
            && body
                .get("errors")
                .and_then(|v| v.as_array())
                .is_none_or(|e| e.is_empty())
        {
            (st, body)
        } else {
            // Fallback: scan recent issues for matching identifier/id.
            let q = r#"
              query {
                issues(first: 50) {
                  nodes {
                    id identifier title description url priority createdAt updatedAt
                    state { name type }
                    assignee { name email }
                    team { key name }
                  }
                }
              }
            "#;
            graphql(http, token, q, json!({})).await
        }
    };

    if !(200..300).contains(&status) {
        return upstream_err("get_issue", status, &body);
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
            "get_issue",
            "upstream",
            format!("Linear GraphQL: {msg}"),
            Some(status),
        );
    }

    if let Some(issue) = body.pointer("/data/issue")
        && !issue.is_null()
    {
        return ProxyResult::ok_result(
            "linear",
            "get_issue",
            "Fetched Linear issue",
            map_issue(issue),
            Some(status),
        );
    }

    let nodes = body
        .pointer("/data/searchIssues/nodes")
        .or_else(|| body.pointer("/data/issues/nodes"))
        .and_then(|v| v.as_array());
    if let Some(nodes) = nodes {
        if let Some(found) = nodes.iter().find(|n| {
            n.get("identifier").and_then(|v| v.as_str()) == Some(lookup)
                || n.get("id").and_then(|v| v.as_str()) == Some(lookup)
        }) {
            return ProxyResult::ok_result(
                "linear",
                "get_issue",
                format!("Fetched Linear issue {lookup}"),
                map_issue(found),
                Some(status),
            );
        }
        if let Some(first) = nodes.first() {
            return ProxyResult::ok_result(
                "linear",
                "get_issue",
                "Fetched Linear issue",
                map_issue(first),
                Some(status),
            );
        }
    }

    ProxyResult::err(
        "linear",
        "get_issue",
        "not_found",
        "issue not found",
        Some(status),
    )
}

