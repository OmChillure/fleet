//! GitLab write tools (control-plane; OAuth token never returned).

use serde_json::{json, Value};

use super::super::client::*;
use crate::proxy::types::ProxyResult;

pub(crate) async fn create_issue(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let project = match require_project(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("gitlab", "create_issue", "bad_args", e, None),
    };
    let title = match args.get("title").and_then(|v| v.as_str()).map(str::trim) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => {
            return ProxyResult::err("gitlab", "create_issue", "bad_args", "title is required", None);
        }
    };
    let mut payload = json!({ "title": title });
    if let Some(d) = args
        .get("description")
        .or_else(|| args.get("body"))
        .and_then(|v| v.as_str())
    {
        payload["description"] = json!(d);
    }
    if let Some(labels) = args.get("labels") {
        payload["labels"] = labels.clone();
    }
    let (status, body) = gitlab_post(
        http,
        token,
        &project_url(&project, "/issues"),
        &payload,
    )
    .await;
    if !(200..300).contains(&status) {
        return upstream_err("create_issue", status, &body);
    }
    ProxyResult::ok_result(
        "gitlab",
        "create_issue",
        format!(
            "Created issue !{} on {project}",
            body.get("iid").and_then(|v| v.as_u64()).unwrap_or(0)
        ),
        map_issue(&body),
        Some(status),
    )
}

pub(crate) async fn add_comment(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let project = match require_project(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("gitlab", "add_comment", "bad_args", e, None),
    };
    let iid = match args
        .get("iid")
        .or_else(|| args.get("number"))
        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
    {
        Some(n) => n,
        None => {
            return ProxyResult::err("gitlab", "add_comment", "bad_args", "iid is required", None);
        }
    };
    let body_text = match args
        .get("body")
        .or_else(|| args.get("comment"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(t) => t.to_string(),
        None => {
            return ProxyResult::err("gitlab", "add_comment", "bad_args", "body is required", None);
        }
    };
    let (status, body) = gitlab_post(
        http,
        token,
        &project_url(&project, &format!("/issues/{iid}/notes")),
        &json!({ "body": body_text }),
    )
    .await;
    if !(200..300).contains(&status) {
        return upstream_err("add_comment", status, &body);
    }
    ProxyResult::ok_result(
        "gitlab",
        "add_comment",
        format!("Commented on issue !{iid} in {project}"),
        json!({
            "id": body.get("id"),
            "iid": iid,
            "body": body.get("body"),
            "created_at": body.get("created_at"),
        }),
        Some(status),
    )
}

pub(crate) async fn create_branch(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let project = match require_project(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("gitlab", "create_branch", "bad_args", e, None),
    };
    let branch = match args.get("branch").and_then(|v| v.as_str()).map(str::trim) {
        Some(b) if !b.is_empty() => b.to_string(),
        _ => {
            return ProxyResult::err(
                "gitlab",
                "create_branch",
                "bad_args",
                "branch is required",
                None,
            );
        }
    };
    let (st, proj) = gitlab_get(http, token, &project_url(&project, "")).await;
    if !(200..300).contains(&st) {
        return upstream_err("create_branch", st, &proj);
    }
    let default_branch = proj
        .get("default_branch")
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    let from = args
        .get("from")
        .or_else(|| args.get("ref"))
        .or_else(|| args.get("base_branch"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_branch);
    let (status, body) = gitlab_post(
        http,
        token,
        &project_url(&project, "/repository/branches"),
        &json!({ "branch": branch, "ref": from }),
    )
    .await;
    if status == 400
        && body
            .get("message")
            .and_then(|v| v.as_str())
            .is_some_and(|m| m.to_ascii_lowercase().contains("already exists"))
    {
        return ProxyResult::ok_result(
            "gitlab",
            "create_branch",
            format!("Branch `{branch}` already exists on {project}"),
            json!({ "project": project, "branch": branch, "existed": true }),
            Some(status),
        );
    }
    if !(200..300).contains(&status) {
        return upstream_err("create_branch", status, &body);
    }
    ProxyResult::ok_result(
        "gitlab",
        "create_branch",
        format!("Created branch `{branch}` on {project}"),
        json!({
            "project": project,
            "branch": body.get("name"),
            "commit": body.get("commit").and_then(|c| c.get("id")),
            "web_url": body.get("web_url"),
        }),
        Some(status),
    )
}

pub(crate) async fn commit_files(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let project = match require_project(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("gitlab", "commit_files", "bad_args", e, None),
    };
    let branch = args
        .get("branch")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("agent-fleet/changes")
        .to_string();
    let message = args
        .get("message")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("chore: apply files via Agent Fleet")
        .to_string();
    let files = match args.get("files").and_then(|v| v.as_array()) {
        Some(a) if !a.is_empty() => a,
        _ => {
            return ProxyResult::err(
                "gitlab",
                "commit_files",
                "bad_args",
                "files must be a non-empty array of {path, content}",
                None,
            );
        }
    };
    if files.len() > MAX_COMMIT_FILES {
        return ProxyResult::err(
            "gitlab",
            "commit_files",
            "bad_args",
            format!("too many files (max {MAX_COMMIT_FILES})"),
            None,
        );
    }

    let (st, proj) = gitlab_get(http, token, &project_url(&project, "")).await;
    if !(200..300).contains(&st) {
        return upstream_err("commit_files", st, &proj);
    }
    let default_branch = proj
        .get("default_branch")
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    let base_branch = args
        .get("base_branch")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_branch);

    let branch_url = format!(
        "{}/repository/branches/{}",
        project_url(&project, ""),
        urlencoding(&branch)
    );
    let (br_st, _) = gitlab_get(http, token, &branch_url).await;
    let branch_exists = (200..300).contains(&br_st);

    let mut actions = Vec::new();
    let mut paths = Vec::new();
    for f in files {
        let path = match f.get("path").and_then(|v| v.as_str()).map(str::trim) {
            Some(p) if !p.is_empty() && !p.starts_with('/') => p,
            _ => {
                return ProxyResult::err(
                    "gitlab",
                    "commit_files",
                    "bad_args",
                    "each file needs a relative path",
                    None,
                );
            }
        };
        let content = match f.get("content").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => {
                return ProxyResult::err(
                    "gitlab",
                    "commit_files",
                    "bad_args",
                    format!("file `{path}` missing content"),
                    None,
                );
            }
        };
        if content.len() > MAX_FILE_BYTES {
            return ProxyResult::err(
                "gitlab",
                "commit_files",
                "bad_args",
                format!("file `{path}` too large (max {MAX_FILE_BYTES} bytes)"),
                None,
            );
        }
        let ref_name = if branch_exists { branch.as_str() } else { base_branch };
        let file_url = format!(
            "{}/repository/files/{}?ref={}",
            project_url(&project, ""),
            path_enc(path),
            urlencoding(ref_name)
        );
        let (fst, _) = gitlab_get(http, token, &file_url).await;
        let action = if (200..300).contains(&fst) {
            "update"
        } else {
            "create"
        };
        actions.push(json!({
            "action": action,
            "file_path": path,
            "content": content,
        }));
        paths.push(path.to_string());
    }

    let mut payload = json!({
        "branch": branch,
        "commit_message": message,
        "actions": actions,
    });
    if !branch_exists {
        payload["start_branch"] = json!(base_branch);
    }

    let (status, body) = gitlab_post(
        http,
        token,
        &project_url(&project, "/repository/commits"),
        &payload,
    )
    .await;
    if !(200..300).contains(&status) {
        return upstream_err("commit_files", status, &body);
    }
    ProxyResult::ok_result(
        "gitlab",
        "commit_files",
        format!(
            "Committed {} file(s) to {project}@{branch}",
            paths.len()
        ),
        json!({
            "project": project,
            "branch": branch,
            "base_branch": base_branch,
            "commit_sha": body.get("id"),
            "web_url": body.get("web_url"),
            "files": paths,
            "hint": "Use create_mr to open a merge request from this branch.",
        }),
        Some(status),
    )
}

pub(crate) async fn create_mr(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let project = match require_project(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("gitlab", "create_mr", "bad_args", e, None),
    };
    let title = match args.get("title").and_then(|v| v.as_str()).map(str::trim) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => {
            return ProxyResult::err("gitlab", "create_mr", "bad_args", "title is required", None);
        }
    };
    let source = match args
        .get("source_branch")
        .or_else(|| args.get("head"))
        .or_else(|| args.get("branch"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(s) => s.to_string(),
        None => {
            return ProxyResult::err(
                "gitlab",
                "create_mr",
                "bad_args",
                "source_branch is required",
                None,
            );
        }
    };
    let (st, proj) = gitlab_get(http, token, &project_url(&project, "")).await;
    if !(200..300).contains(&st) {
        return upstream_err("create_mr", st, &proj);
    }
    let default_branch = proj
        .get("default_branch")
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    let target = args
        .get("target_branch")
        .or_else(|| args.get("base"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_branch)
        .to_string();
    let description = args
        .get("description")
        .or_else(|| args.get("body"))
        .and_then(|v| v.as_str())
        .unwrap_or("Opened by Agent Fleet (control-plane integration; no tokens in sandboxes).");
    let draft = args.get("draft").and_then(|v| v.as_bool()).unwrap_or(false);

    let (status, body) = gitlab_post(
        http,
        token,
        &project_url(&project, "/merge_requests"),
        &json!({
            "title": title,
            "source_branch": source,
            "target_branch": target,
            "description": description,
            "draft": draft,
        }),
    )
    .await;
    if !(200..300).contains(&status) {
        return upstream_err("create_mr", status, &body);
    }
    ProxyResult::ok_result(
        "gitlab",
        "create_mr",
        format!(
            "Opened MR !{}: {}",
            body.get("iid").and_then(|v| v.as_u64()).unwrap_or(0),
            title
        ),
        map_mr(&body),
        Some(status),
    )
}
