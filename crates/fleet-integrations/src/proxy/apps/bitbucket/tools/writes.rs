//! Bitbucket write tools.

use serde_json::{json, Value};

use super::super::client::*;
use crate::proxy::types::ProxyResult;

pub(crate) async fn create_branch(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
) -> ProxyResult {
    let (ws, repo) = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("bitbucket", "create_branch", "bad_args", e, None),
    };
    let name = args
        .get("name")
        .or_else(|| args.get("branch"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    let target = args
        .get("from")
        .or_else(|| args.get("target"))
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD");
    if name.is_empty() {
        return ProxyResult::err(
            "bitbucket",
            "create_branch",
            "bad_args",
            "name is required",
            None,
        );
    }
    let url = repo_url(&ws, &repo, "/refs/branches");
    let payload = json!({
        "name": name,
        "target": { "hash": target }
    });
    let (status, body) = bb_post(http, token, &url, &payload).await;
    if !(200..300).contains(&status) {
        return upstream_err("create_branch", status, &body);
    }
    ProxyResult::ok_result(
        "bitbucket",
        "create_branch",
        format!("Created branch {name} on {ws}/{repo}"),
        json!({ "name": name, "target": target, "raw": body }),
        Some(status),
    )
}

pub(crate) async fn commit_files(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (ws, repo) = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("bitbucket", "commit_files", "bad_args", e, None),
    };
    let message = args
        .get("message")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("chore: update via Agent Fleet");
    let branch = args
        .get("branch")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let files = args
        .get("files")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if files.is_empty() {
        return ProxyResult::err(
            "bitbucket",
            "commit_files",
            "bad_args",
            "files is required ([{path,content}])",
            None,
        );
    }
    let mut form: Vec<(String, String)> = vec![("message".into(), message.to_string())];
    if let Some(b) = branch {
        form.push(("branch".into(), b.to_string()));
    }
    for f in &files {
        let path = f.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let content = f.get("content").and_then(|v| v.as_str()).unwrap_or("");
        if path.is_empty() {
            continue;
        }
        form.push((path.to_string(), content.to_string()));
    }
    let url = repo_url(&ws, &repo, "/src");
    let encoded = form
        .iter()
        .map(|(k, v)| format!("{}={}", form_enc(k), form_enc(v)))
        .collect::<Vec<_>>()
        .join("&");
    let res = http
        .post(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(encoded)
        .send()
        .await;
    let (status, body) = match res {
        Ok(r) => {
            let status = r.status().as_u16();
            let body = r.json::<Value>().await.unwrap_or(json!({}));
            (status, body)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    };
    if !(200..300).contains(&status) {
        return upstream_err("commit_files", status, &body);
    }
    ProxyResult::ok_result(
        "bitbucket",
        "commit_files",
        format!("Committed {} file(s) on {ws}/{repo}", files.len()),
        json!({ "workspace": ws, "repo": repo, "message": message, "raw": body }),
        Some(status),
    )
}

pub(crate) async fn create_pr(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (ws, repo) = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("bitbucket", "create_pr", "bad_args", e, None),
    };
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    let source = args
        .get("source")
        .or_else(|| args.get("head"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let dest = args
        .get("destination")
        .or_else(|| args.get("base"))
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    if title.is_empty() || source.is_empty() {
        return ProxyResult::err(
            "bitbucket",
            "create_pr",
            "bad_args",
            "title and source (head branch) are required",
            None,
        );
    }
    let description = args
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let url = repo_url(&ws, &repo, "/pullrequests");
    let payload = json!({
        "title": title,
        "description": { "raw": description },
        "source": { "branch": { "name": source } },
        "destination": { "branch": { "name": dest } },
        "close_source_branch": false
    });
    let (status, body) = bb_post(http, token, &url, &payload).await;
    if !(200..300).contains(&status) {
        return upstream_err("create_pr", status, &body);
    }
    ProxyResult::ok_result(
        "bitbucket",
        "create_pr",
        format!("Opened PR on {ws}/{repo}"),
        map_pr(&body),
        Some(status),
    )
}

pub(crate) async fn create_issue(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (ws, repo) = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("bitbucket", "create_issue", "bad_args", e, None),
    };
    let title = args
        .get("title")
        .or_else(|| args.get("summary"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    if title.is_empty() {
        return ProxyResult::err(
            "bitbucket",
            "create_issue",
            "bad_args",
            "title is required",
            None,
        );
    }
    let content = args
        .get("content")
        .or_else(|| args.get("body"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let url = repo_url(&ws, &repo, "/issues");
    let payload = json!({
        "title": title,
        "content": { "raw": content }
    });
    let (status, body) = bb_post(http, token, &url, &payload).await;
    if !(200..300).contains(&status) {
        return upstream_err("create_issue", status, &body);
    }
    ProxyResult::ok_result(
        "bitbucket",
        "create_issue",
        format!("Created issue on {ws}/{repo}"),
        map_issue(&body),
        Some(status),
    )
}

pub(crate) async fn add_comment(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (ws, repo) = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("bitbucket", "add_comment", "bad_args", e, None),
    };
    let text = args
        .get("text")
        .or_else(|| args.get("body"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    if text.is_empty() {
        return ProxyResult::err(
            "bitbucket",
            "add_comment",
            "bad_args",
            "text is required",
            None,
        );
    }
    let suffix = if let Some(id) =
        args.get("pr")
            .or_else(|| args.get("pullrequest"))
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
            }) {
        format!("/pullrequests/{id}/comments")
    } else if let Some(id) = args.get("issue").and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    }) {
        format!("/issues/{id}/comments")
    } else {
        return ProxyResult::err(
            "bitbucket",
            "add_comment",
            "bad_args",
            "pr or issue id is required",
            None,
        );
    };
    let url = repo_url(&ws, &repo, &suffix);
    let payload = json!({ "content": { "raw": text } });
    let (status, body) = bb_post(http, token, &url, &payload).await;
    if !(200..300).contains(&status) {
        return upstream_err("add_comment", status, &body);
    }
    ProxyResult::ok_result(
        "bitbucket",
        "add_comment",
        "Added comment",
        json!({ "ok": true, "raw": body }),
        Some(status),
    )
}

fn form_enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
