//! Figma file / project read tools. Full file trees are truncated.

use serde_json::{Value, json};

use super::super::client::*;
use crate::proxy::types::ProxyResult;

pub(crate) async fn get_me(http: &reqwest::Client, token: &str, _args: &Value) -> ProxyResult {
    let (status, body) = figma_get(http, token, "https://api.figma.com/v1/me").await;
    if !(200..300).contains(&status) {
        return upstream_err("get_me", status, &body);
    }
    ProxyResult::ok_result(
        "figma",
        "get_me",
        "Fetched Figma user",
        json!({
            "id": body.get("id"),
            "email": body.get("email"),
            "handle": body.get("handle"),
        }),
        Some(status),
    )
}

pub(crate) async fn get_file(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let key = match req_str(args, "file_key").or_else(|_| req_str(args, "key")) {
        Ok(k) => k,
        Err(e) => return ProxyResult::err("figma", "get_file", "bad_args", e, None),
    };
    let url = format!("https://api.figma.com/v1/files/{key}?depth=1");
    let (status, body) = figma_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("get_file", status, &body);
    }
    let pages = page_summaries(body.get("document").unwrap_or(&Value::Null));
    ProxyResult::ok_result(
        "figma",
        "get_file",
        format!("Fetched Figma file {key} metadata"),
        json!({
            "file_key": key,
            "name": body.get("name"),
            "lastModified": body.get("lastModified"),
            "version": body.get("version"),
            "thumbnailUrl": body.get("thumbnailUrl"),
            "role": body.get("role"),
            "pages": pages,
        }),
        Some(status),
    )
}

pub(crate) async fn get_file_nodes(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
) -> ProxyResult {
    let key = match req_str(args, "file_key").or_else(|_| req_str(args, "key")) {
        Ok(k) => k,
        Err(e) => return ProxyResult::err("figma", "get_file_nodes", "bad_args", e, None),
    };
    let ids = args
        .get("ids")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .or_else(|| {
            args.get("ids").and_then(|v| v.as_array()).map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str())
                    .collect::<Vec<_>>()
                    .join(",")
            })
        });
    let Some(ids) = ids.filter(|s| !s.is_empty()) else {
        return ProxyResult::err(
            "figma",
            "get_file_nodes",
            "bad_args",
            "ids is required (comma string or array of node ids)",
            None,
        );
    };
    let url = format!("https://api.figma.com/v1/files/{key}/nodes?ids={ids}&depth=1");
    let (status, body) = figma_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("get_file_nodes", status, &body);
    }
    let nodes = body.get("nodes").cloned().unwrap_or(json!({}));
    let summaries: Vec<Value> = nodes
        .as_object()
        .map(|map| {
            map.iter()
                .map(|(id, node)| {
                    let doc = node.get("document").unwrap_or(node);
                    json!({
                        "id": id,
                        "name": doc.get("name"),
                        "type": doc.get("type"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    ProxyResult::ok_result(
        "figma",
        "get_file_nodes",
        format!("Fetched {} Figma nodes from {key}", summaries.len()),
        json!({ "file_key": key, "items": summaries }),
        Some(status),
    )
}

pub(crate) async fn list_comments(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
) -> ProxyResult {
    let key = match req_str(args, "file_key").or_else(|_| req_str(args, "key")) {
        Ok(k) => k,
        Err(e) => return ProxyResult::err("figma", "list_comments", "bad_args", e, None),
    };
    let url = format!("https://api.figma.com/v1/files/{key}/comments");
    let (status, body) = figma_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_comments", status, &body);
    }
    let items: Vec<Value> = body
        .get("comments")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|c| {
                    json!({
                        "id": c.get("id"),
                        "message": c.get("message"),
                        "user": c.get("user").and_then(|u| u.get("handle")),
                        "created_at": c.get("created_at"),
                        "resolved_at": c.get("resolved_at"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "figma",
        "list_comments",
        format!("Listed {n} Figma comments on {key}"),
        json!({ "file_key": key, "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn list_team_projects(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
) -> ProxyResult {
    let team_id = match req_str(args, "team_id") {
        Ok(t) => t,
        Err(e) => {
            return ProxyResult::err("figma", "list_team_projects", "bad_args", e, None);
        }
    };
    let url = format!("https://api.figma.com/v1/teams/{team_id}/projects");
    let (status, body) = figma_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_team_projects", status, &body);
    }
    let items: Vec<Value> = body
        .get("projects")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|p| json!({ "id": p.get("id"), "name": p.get("name") }))
                .collect()
        })
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "figma",
        "list_team_projects",
        format!("Listed {n} Figma projects for team {team_id}"),
        json!({
            "team_id": team_id,
            "name": body.get("name"),
            "count": n,
            "items": items
        }),
        Some(status),
    )
}

pub(crate) async fn get_project_files(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
) -> ProxyResult {
    let project_id = match req_str(args, "project_id") {
        Ok(p) => p,
        Err(e) => {
            return ProxyResult::err("figma", "get_project_files", "bad_args", e, None);
        }
    };
    let url = format!("https://api.figma.com/v1/projects/{project_id}/files");
    let (status, body) = figma_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("get_project_files", status, &body);
    }
    let items: Vec<Value> = body
        .get("files")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .map(|f| {
                    json!({
                        "key": f.get("key"),
                        "name": f.get("name"),
                        "last_modified": f.get("last_modified"),
                        "thumbnail_url": f.get("thumbnail_url"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "figma",
        "get_project_files",
        format!("Listed {n} Figma files in project {project_id}"),
        json!({ "project_id": project_id, "count": n, "items": items }),
        Some(status),
    )
}
