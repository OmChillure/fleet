//! GitLab read tools.

use serde_json::{json, Value};

use super::super::client::*;
use crate::proxy::types::ProxyResult;

pub(crate) async fn list_repos(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let per_page = args
        .get("per_page")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50);
    let url = format!(
        "https://gitlab.com/api/v4/projects?membership=true&simple=true&order_by=last_activity_at&sort=desc&per_page={per_page}"
    );
    let (status, body) = gitlab_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_repos", status, &body);
    }
    let items: Vec<Value> = body
        .as_array()
        .map(|arr| arr.iter().map(map_project).collect())
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "gitlab",
        "list_repos",
        format!("Listed {n} GitLab projects"),
        json!({ "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn list_issues(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let project = match require_project(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("gitlab", "list_issues", "bad_args", e, None),
    };
    let state = args.get("state").and_then(|v| v.as_str()).unwrap_or("opened");
    let per_page = args
        .get("per_page")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50);
    let url = format!(
        "{}?state={}&per_page={per_page}",
        project_url(&project, "/issues"),
        urlencoding(state)
    );
    let (status, body) = gitlab_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_issues", status, &body);
    }
    let items: Vec<Value> = body
        .as_array()
        .map(|arr| arr.iter().map(map_issue).collect())
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "gitlab",
        "list_issues",
        format!("Listed {n} issues on {project}"),
        json!({ "project": project, "count": n, "items": items }),
        Some(status),
    )
}

fn issue_iid(args: &Value) -> Result<u64, String> {
    args.get("iid")
        .or_else(|| args.get("number"))
        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .ok_or_else(|| "iid is required".into())
}

pub(crate) async fn get_issue(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let project = match require_project(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("gitlab", "get_issue", "bad_args", e, None),
    };
    let iid = match issue_iid(args) {
        Ok(n) => n,
        Err(e) => return ProxyResult::err("gitlab", "get_issue", "bad_args", e, None),
    };
    let url = project_url(&project, &format!("/issues/{iid}"));
    let (status, body) = gitlab_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("get_issue", status, &body);
    }
    ProxyResult::ok_result(
        "gitlab",
        "get_issue",
        format!("Fetched issue !{iid} on {project}"),
        map_issue(&body),
        Some(status),
    )
}

pub(crate) async fn list_mrs(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let project = match require_project(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("gitlab", "list_mrs", "bad_args", e, None),
    };
    let state = args.get("state").and_then(|v| v.as_str()).unwrap_or("opened");
    let per_page = args
        .get("per_page")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50);
    let url = format!(
        "{}?state={}&per_page={per_page}",
        project_url(&project, "/merge_requests"),
        urlencoding(state)
    );
    let (status, body) = gitlab_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_mrs", status, &body);
    }
    let items: Vec<Value> = body
        .as_array()
        .map(|arr| arr.iter().map(map_mr).collect())
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "gitlab",
        "list_mrs",
        format!("Listed {n} merge requests on {project}"),
        json!({ "project": project, "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn get_mr(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let project = match require_project(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("gitlab", "get_mr", "bad_args", e, None),
    };
    let iid = match issue_iid(args) {
        Ok(n) => n,
        Err(e) => return ProxyResult::err("gitlab", "get_mr", "bad_args", e, None),
    };
    let url = project_url(&project, &format!("/merge_requests/{iid}"));
    let (status, body) = gitlab_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("get_mr", status, &body);
    }
    ProxyResult::ok_result(
        "gitlab",
        "get_mr",
        format!("Fetched MR !{iid} on {project}"),
        map_mr(&body),
        Some(status),
    )
}

pub(crate) async fn get_file(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let project = match require_project(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("gitlab", "get_file", "bad_args", e, None),
    };
    let path = match args.get("path").and_then(|v| v.as_str()).map(str::trim) {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => {
            return ProxyResult::err("gitlab", "get_file", "bad_args", "path is required", None);
        }
    };
    let mut url = format!(
        "{}/repository/files/{}",
        project_url(&project, ""),
        path_enc(&path)
    );
    if let Some(r) = args
        .get("ref")
        .or_else(|| args.get("branch"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        url.push_str(&format!("?ref={}", urlencoding(r)));
    } else {
        url.push_str("?ref=HEAD");
    }
    let (status, body) = gitlab_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("get_file", status, &body);
    }
    let encoding = body.get("encoding").and_then(|v| v.as_str()).unwrap_or("");
    let mut content = String::new();
    if encoding == "base64" {
        if let Some(raw) = body.get("content").and_then(|v| v.as_str()) {
            let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
            if let Ok(bytes) = {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD.decode(compact.as_bytes())
            } {
                content = String::from_utf8_lossy(&bytes).into_owned();
                if content.len() > 16_384 {
                    content = format!("{}…", &content[..16_384]);
                }
            }
        }
    }
    ProxyResult::ok_result(
        "gitlab",
        "get_file",
        format!("Fetched {path} from {project}"),
        json!({
            "project": project,
            "path": body.get("file_path"),
            "ref": body.get("ref"),
            "size": body.get("size"),
            "last_commit_id": body.get("last_commit_id"),
            "content": content,
        }),
        Some(status),
    )
}

pub(crate) async fn list_commits(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let project = match require_project(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("gitlab", "list_commits", "bad_args", e, None),
    };
    let per_page = args
        .get("per_page")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50);
    let mut url = format!(
        "{}/repository/commits?per_page={per_page}",
        project_url(&project, "")
    );
    if let Some(r) = args
        .get("ref")
        .or_else(|| args.get("branch"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        url.push_str(&format!("&ref_name={}", urlencoding(r)));
    }
    let (status, body) = gitlab_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_commits", status, &body);
    }
    let items: Vec<Value> = body
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|c| {
                    json!({
                        "id": c.get("id"),
                        "short_id": c.get("short_id"),
                        "title": c.get("title"),
                        "author_name": c.get("author_name"),
                        "committed_date": c.get("committed_date"),
                        "web_url": c.get("web_url"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "gitlab",
        "list_commits",
        format!("Listed {n} commits on {project}"),
        json!({ "project": project, "count": n, "items": items }),
        Some(status),
    )
}
