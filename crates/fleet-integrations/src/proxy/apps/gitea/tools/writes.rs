//! Gitea write tools. The token stays on the control plane.

use serde_json::{json, Value};

use super::super::client::*;
use crate::proxy::types::ProxyResult;

fn bad(op: &str, e: impl Into<String>) -> ProxyResult {
    ProxyResult::err("gitea", op, "bad_args", e.into(), None)
}

fn upstream(op: &str, status: u16, body: &Value) -> ProxyResult {
    let msg = body
        .get("message")
        .or_else(|| body.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("gitea request failed");
    ProxyResult::err(
        "gitea",
        op,
        "upstream",
        format!("{msg} ({status})"),
        Some(status),
    )
}

pub(crate) async fn create_issue(
    http: &reqwest::Client,
    token: &str,
    base: &str,
    args: &Value,
) -> ProxyResult {
    let repo = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return bad("create_issue", e),
    };
    let title = match args.get("title").and_then(|v| v.as_str()).map(str::trim) {
        Some(t) if !t.is_empty() => t,
        _ => return bad("create_issue", "title is required"),
    };
    let mut payload = json!({ "title": title });
    if let Some(d) = args
        .get("body")
        .or_else(|| args.get("description"))
        .and_then(|v| v.as_str())
    {
        payload["body"] = json!(d);
    }
    let url = match repo_api(base, &repo, "/issues") {
        Ok(u) => u,
        Err(e) => return bad("create_issue", e),
    };
    let (status, body) = gitea_send(http, token, reqwest::Method::POST, &url, Some(&payload)).await;
    if !(200..300).contains(&status) {
        return upstream("create_issue", status, &body);
    }
    let n = body.get("number").and_then(|v| v.as_u64()).unwrap_or(0);
    ProxyResult::ok_result(
        "gitea",
        "create_issue",
        format!("Created issue #{n} on {repo}"),
        map_issue(&body),
        Some(status),
    )
}

pub(crate) async fn update_issue(
    http: &reqwest::Client,
    token: &str,
    base: &str,
    args: &Value,
) -> ProxyResult {
    let repo = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return bad("update_issue", e),
    };
    let n = match args
        .get("number")
        .or_else(|| args.get("iid"))
        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
    {
        Some(n) => n,
        None => return bad("update_issue", "number is required"),
    };
    let mut payload = json!({});
    if let Some(t) = args.get("title").and_then(|v| v.as_str()) {
        payload["title"] = json!(t);
    }
    if let Some(b) = args.get("body").or_else(|| args.get("description")).and_then(|v| v.as_str()) {
        payload["body"] = json!(b);
    }
    if let Some(state) = args.get("state").and_then(|v| v.as_str()) {
        payload["state"] = json!(state);
    }
    let url = match repo_api(base, &repo, &format!("/issues/{n}")) {
        Ok(u) => u,
        Err(e) => return bad("update_issue", e),
    };
    let (status, body) = gitea_send(http, token, reqwest::Method::PATCH, &url, Some(&payload)).await;
    if !(200..300).contains(&status) {
        return upstream("update_issue", status, &body);
    }
    ProxyResult::ok_result(
        "gitea",
        "update_issue",
        format!("Updated issue #{n} on {repo}"),
        map_issue(&body),
        Some(status),
    )
}

pub(crate) async fn add_comment(
    http: &reqwest::Client,
    token: &str,
    base: &str,
    args: &Value,
) -> ProxyResult {
    let repo = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return bad("add_comment", e),
    };
    let n = match args
        .get("number")
        .or_else(|| args.get("iid"))
        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
    {
        Some(n) => n,
        None => return bad("add_comment", "number is required"),
    };
    let body_text = match args.get("body").and_then(|v| v.as_str()).map(str::trim) {
        Some(t) if !t.is_empty() => t,
        _ => return bad("add_comment", "body is required"),
    };
    let url = match repo_api(base, &repo, &format!("/issues/{n}/comments")) {
        Ok(u) => u,
        Err(e) => return bad("add_comment", e),
    };
    let (status, body) = gitea_send(
        http,
        token,
        reqwest::Method::POST,
        &url,
        Some(&json!({ "body": body_text })),
    )
    .await;
    if !(200..300).contains(&status) {
        return upstream("add_comment", status, &body);
    }
    ProxyResult::ok_result(
        "gitea",
        "add_comment",
        format!("Commented on issue #{n} of {repo}"),
        json!({ "id": body.get("id"), "html_url": body.get("html_url") }),
        Some(status),
    )
}

pub(crate) async fn create_branch(
    http: &reqwest::Client,
    token: &str,
    base: &str,
    args: &Value,
) -> ProxyResult {
    let repo = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return bad("create_branch", e),
    };
    let name = match args.get("branch").or_else(|| args.get("name")).and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.trim(),
        _ => return bad("create_branch", "branch is required"),
    };
    let from = args
        .get("from")
        .or_else(|| args.get("ref"))
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    let url = match repo_api(base, &repo, "/branches") {
        Ok(u) => u,
        Err(e) => return bad("create_branch", e),
    };
    let payload = json!({ "new_branch_name": name, "old_branch_name": from });
    let (status, body) = gitea_send(http, token, reqwest::Method::POST, &url, Some(&payload)).await;
    if !(200..300).contains(&status) {
        return upstream("create_branch", status, &body);
    }
    ProxyResult::ok_result(
        "gitea",
        "create_branch",
        format!("Created branch {name} on {repo} from {from}"),
        json!({ "repo": repo, "branch": name, "from": from }),
        Some(status),
    )
}

pub(crate) async fn commit_files(
    http: &reqwest::Client,
    token: &str,
    base: &str,
    args: &Value,
) -> ProxyResult {
    let repo = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return bad("commit_files", e),
    };
    let message = match args.get("message").and_then(|v| v.as_str()).map(str::trim) {
        Some(m) if !m.is_empty() => m,
        _ => return bad("commit_files", "message is required"),
    };
    let branch = args.get("branch").and_then(|v| v.as_str()).unwrap_or("main");
    let files = match args.get("files").and_then(|v| v.as_array()) {
        Some(f) if !f.is_empty() => f,
        _ => return bad("commit_files", "files is required"),
    };
    if files.len() > MAX_COMMIT_FILES {
        return bad("commit_files", format!("at most {MAX_COMMIT_FILES} files"));
    }
    let mut changes = Vec::new();
    for f in files {
        let path = match f.get("path").and_then(|v| v.as_str()).map(str::trim) {
            Some(p) if !p.is_empty() => p,
            _ => return bad("commit_files", "each file needs path"),
        };
        let content = f.get("content").and_then(|v| v.as_str()).unwrap_or("");
        if content.len() > MAX_FILE_BYTES {
            return bad("commit_files", format!("{path} exceeds {MAX_FILE_BYTES} bytes"));
        }
        let operation = f.get("operation").and_then(|v| v.as_str()).unwrap_or("create");
        changes.push(json!({
            "operation": operation,
            "path": path,
            "content": b64_encode(content.as_bytes()),
        }));
    }
    let url = match repo_api(base, &repo, "/contents") {
        Ok(u) => u,
        Err(e) => return bad("commit_files", e),
    };
    let payload = json!({ "branch": branch, "message": message, "files": changes });
    let (status, body) = gitea_send(http, token, reqwest::Method::POST, &url, Some(&payload)).await;
    if !(200..300).contains(&status) {
        return upstream("commit_files", status, &body);
    }
    ProxyResult::ok_result(
        "gitea",
        "commit_files",
        format!("Committed {} file(s) to {branch} on {repo}", changes.len()),
        json!({
            "repo": repo,
            "branch": branch,
            "sha": body.get("commit").and_then(|c| c.get("sha")).or_else(|| body.get("sha")),
        }),
        Some(status),
    )
}

pub(crate) async fn create_mr(
    http: &reqwest::Client,
    token: &str,
    base: &str,
    args: &Value,
) -> ProxyResult {
    let repo = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return bad("create_mr", e),
    };
    let title = match args.get("title").and_then(|v| v.as_str()).map(str::trim) {
        Some(t) if !t.is_empty() => t,
        _ => return bad("create_mr", "title is required"),
    };
    let head = match args
        .get("head")
        .or_else(|| args.get("source_branch"))
        .and_then(|v| v.as_str())
    {
        Some(s) if !s.is_empty() => s,
        _ => return bad("create_mr", "head (source branch) is required"),
    };
    let base_branch = args
        .get("base")
        .or_else(|| args.get("target_branch"))
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    let mut payload = json!({ "title": title, "head": head, "base": base_branch });
    if let Some(b) = args.get("body").or_else(|| args.get("description")).and_then(|v| v.as_str()) {
        payload["body"] = json!(b);
    }
    let url = match repo_api(base, &repo, "/pulls") {
        Ok(u) => u,
        Err(e) => return bad("create_mr", e),
    };
    let (status, body) = gitea_send(http, token, reqwest::Method::POST, &url, Some(&payload)).await;
    if !(200..300).contains(&status) {
        return upstream("create_mr", status, &body);
    }
    let n = body.get("number").and_then(|v| v.as_u64()).unwrap_or(0);
    ProxyResult::ok_result(
        "gitea",
        "create_mr",
        format!("Opened pull request #{n} on {repo}"),
        map_pr(&body),
        Some(status),
    )
}

pub(crate) async fn merge_mr(
    http: &reqwest::Client,
    token: &str,
    base: &str,
    args: &Value,
) -> ProxyResult {
    let repo = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return bad("merge_mr", e),
    };
    let n = match args
        .get("number")
        .or_else(|| args.get("iid"))
        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
    {
        Some(n) => n,
        None => return bad("merge_mr", "number is required"),
    };
    let url = match repo_api(base, &repo, &format!("/pulls/{n}/merge")) {
        Ok(u) => u,
        Err(e) => return bad("merge_mr", e),
    };
    let (status, body) = gitea_send(
        http,
        token,
        reqwest::Method::POST,
        &url,
        Some(&json!({ "Do": "merge" })),
    )
    .await;
    if !(200..300).contains(&status) {
        return upstream("merge_mr", status, &body);
    }
    ProxyResult::ok_result(
        "gitea",
        "merge_mr",
        format!("Merged pull request #{n} on {repo}"),
        json!({ "repo": repo, "number": n }),
        Some(status),
    )
}

fn b64_encode(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        let n = ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8) | data[i + 2] as u32;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(T[((n >> 6) & 63) as usize] as char);
        out.push(T[(n & 63) as usize] as char);
        i += 3;
    }
    if i < data.len() {
        let rem = data.len() - i;
        let n = if rem == 1 {
            (data[i] as u32) << 16
        } else {
            ((data[i] as u32) << 16) | ((data[i + 1] as u32) << 8)
        };
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        if rem == 2 {
            out.push(T[((n >> 6) & 63) as usize] as char);
            out.push('=');
        } else {
            out.push('=');
            out.push('=');
        }
    }
    out
}
