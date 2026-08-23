//! GitHub read tools.

use serde_json::{json, Value};

use crate::proxy::types::ProxyResult;
use super::super::client::*;

pub(crate) async fn list_repos(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let per_page = args
        .get("per_page")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50);
    let url = format!("https://api.github.com/user/repos?per_page={per_page}&sort=updated");
    let (status, body) = github_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_repos", status, &body);
    }
    let items: Vec<Value> = body
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|r| {
                    json!({
                        "full_name": r.get("full_name"),
                        "private": r.get("private"),
                        "html_url": r.get("html_url"),
                        "description": r.get("description"),
                        "default_branch": r.get("default_branch"),
                        "language": r.get("language"),
                        "updated_at": r.get("updated_at"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "github",
        "list_repos",
        format!("Listed {n} GitHub repos"),
        json!({ "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn list_issues(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (owner, repo) = match require_owner_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("github", "list_issues", "bad_args", e, None),
    };
    let state = args
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("open");
    let per_page = args
        .get("per_page")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50);
    let url = format!(
        "https://api.github.com/repos/{owner}/{repo}/issues?state={state}&per_page={per_page}"
    );
    let (status, body) = github_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_issues", status, &body);
    }
    // GitHub includes PRs in /issues — filter pure issues.
    let items: Vec<Value> = body
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|i| i.get("pull_request").is_none())
                .map(map_issue)
                .collect()
        })
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "github",
        "list_issues",
        format!("Listed {n} issues on {owner}/{repo}"),
        json!({ "owner": owner, "repo": repo, "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn get_issue(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (owner, repo) = match require_owner_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("github", "get_issue", "bad_args", e, None),
    };
    let number = match args.get("number").and_then(|v| v.as_u64()).or_else(|| {
        args.get("number")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
    }) {
        Some(n) => n,
        None => {
            return ProxyResult::err(
                "github",
                "get_issue",
                "bad_args",
                "number is required",
                None,
            )
        }
    };
    let url = format!("https://api.github.com/repos/{owner}/{repo}/issues/{number}");
    let (status, body) = github_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("get_issue", status, &body);
    }
    ProxyResult::ok_result(
        "github",
        "get_issue",
        format!("Fetched issue #{number} on {owner}/{repo}"),
        map_issue(&body),
        Some(status),
    )
}

pub(crate) async fn list_prs(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (owner, repo) = match require_owner_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("github", "list_prs", "bad_args", e, None),
    };
    let state = args
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("open");
    let per_page = args
        .get("per_page")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50);
    let url = format!(
        "https://api.github.com/repos/{owner}/{repo}/pulls?state={state}&per_page={per_page}"
    );
    let (status, body) = github_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_prs", status, &body);
    }
    let items: Vec<Value> = body
        .as_array()
        .map(|arr| arr.iter().map(map_pr).collect())
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "github",
        "list_prs",
        format!("Listed {n} PRs on {owner}/{repo}"),
        json!({ "owner": owner, "repo": repo, "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn get_pr(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (owner, repo) = match require_owner_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("github", "get_pr", "bad_args", e, None),
    };
    let number = match args.get("number").and_then(|v| v.as_u64()).or_else(|| {
        args.get("number")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
    }) {
        Some(n) => n,
        None => {
            return ProxyResult::err("github", "get_pr", "bad_args", "number is required", None)
        }
    };
    let url = format!("https://api.github.com/repos/{owner}/{repo}/pulls/{number}");
    let (status, body) = github_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("get_pr", status, &body);
    }
    ProxyResult::ok_result(
        "github",
        "get_pr",
        format!("Fetched PR #{number} on {owner}/{repo}"),
        map_pr(&body),
        Some(status),
    )
}

pub(crate) async fn get_file(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (owner, repo) = match require_owner_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("github", "get_file", "bad_args", e, None),
    };
    let path = match args.get("path").and_then(|v| v.as_str()).map(str::trim) {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => {
            return ProxyResult::err("github", "get_file", "bad_args", "path is required", None);
        }
    };
    let mut url = format!(
        "https://api.github.com/repos/{owner}/{repo}/contents/{}",
        path_enc(&path)
    );
    if let Some(r) = args.get("ref").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        url.push_str(&format!("?ref={}", urlencoding(r)));
    }
    let (status, body) = github_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("get_file", status, &body);
    }
    if body.get("type").and_then(|v| v.as_str()) == Some("dir")
        || body.as_array().is_some()
    {
        let entries: Vec<Value> = body
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|e| {
                json!({
                    "name": e.get("name"),
                    "path": e.get("path"),
                    "type": e.get("type"),
                    "size": e.get("size"),
                })
            })
            .collect();
        return ProxyResult::ok_result(
            "github",
            "get_file",
            format!("Listed directory {path} on {owner}/{repo}"),
            json!({ "owner": owner, "repo": repo, "path": path, "type": "dir", "items": entries }),
            Some(status),
        );
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
        "github",
        "get_file",
        format!("Fetched {path} from {owner}/{repo}"),
        json!({
            "owner": owner,
            "repo": repo,
            "path": body.get("path"),
            "sha": body.get("sha"),
            "size": body.get("size"),
            "html_url": body.get("html_url"),
            "content": content,
        }),
        Some(status),
    )
}

pub(crate) async fn list_commits(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
) -> ProxyResult {
    let (owner, repo) = match require_owner_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("github", "list_commits", "bad_args", e, None),
    };
    let per_page = args
        .get("per_page")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50);
    let mut url = format!(
        "https://api.github.com/repos/{owner}/{repo}/commits?per_page={per_page}"
    );
    if let Some(sha) = args.get("sha").or_else(|| args.get("branch")).and_then(|v| v.as_str()) {
        url.push_str(&format!("&sha={}", urlencoding(sha)));
    }
    let (status, body) = github_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_commits", status, &body);
    }
    let items: Vec<Value> = body
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|c| {
                    json!({
                        "sha": c.get("sha").and_then(|s| s.as_str()).map(|s| {
                            if s.len() > 7 { &s[..7] } else { s }
                        }),
                        "full_sha": c.get("sha"),
                        "message": c.get("commit").and_then(|m| m.get("message")).and_then(|v| v.as_str()).map(|s| truncate_str(s, 200)),
                        "author": c.get("commit").and_then(|m| m.get("author")).and_then(|a| a.get("name")),
                        "date": c.get("commit").and_then(|m| m.get("author")).and_then(|a| a.get("date")),
                        "html_url": c.get("html_url"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "github",
        "list_commits",
        format!("Listed {n} commits on {owner}/{repo}"),
        json!({ "owner": owner, "repo": repo, "count": n, "items": items }),
        Some(status),
    )
}

/// Combined commit status + check runs. `state` is success | pending | failure | none.
pub(crate) async fn list_checks(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (owner, repo) = match require_owner_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("github", "list_checks", "bad_args", e, None),
    };
    let git_ref = args
        .get("ref")
        .or_else(|| args.get("sha"))
        .or_else(|| args.get("branch"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("HEAD");
    let enc = urlencoding(git_ref);
    let (st_status, status_body) = github_get(
        http,
        token,
        &format!("https://api.github.com/repos/{owner}/{repo}/commits/{enc}/status"),
    )
    .await;
    if st_status != 0 && !(200..300).contains(&st_status) {
        return upstream_err("list_checks", st_status, &status_body);
    }
    let (st_runs, runs_body) = github_get(
        http,
        token,
        &format!(
            "https://api.github.com/repos/{owner}/{repo}/commits/{enc}/check-runs?per_page=50"
        ),
    )
    .await;
    if st_runs != 0 && !(200..300).contains(&st_runs) && st_status == 0 {
        return upstream_err("list_checks", st_runs, &runs_body);
    }

    let mut items = Vec::new();
    let mut failing = Vec::new();
    let mut pending = false;
    let mut saw_any = false;

    if let Some(arr) = status_body.get("statuses").and_then(|v| v.as_array()) {
        for s in arr {
            saw_any = true;
            let context = s
                .get("context")
                .and_then(|v| v.as_str())
                .unwrap_or("status")
                .to_string();
            let state = s
                .get("state")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let desc = s
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if state == "pending" {
                pending = true;
            }
            if state == "failure" || state == "error" {
                failing.push(json!({ "name": context, "state": state, "summary": desc }));
            }
            items.push(json!({ "name": context, "state": state, "summary": desc }));
        }
    }

    if let Some(arr) = runs_body.get("check_runs").and_then(|v| v.as_array()) {
        for r in arr {
            saw_any = true;
            let name = r
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("check")
                .to_string();
            let status = r
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let conclusion = r
                .get("conclusion")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let summary = r
                .pointer("/output/summary")
                .or_else(|| r.pointer("/output/title"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if status != "completed" {
                pending = true;
            }
            let bad = matches!(
                conclusion.as_str(),
                "failure" | "timed_out" | "cancelled" | "startup_failure" | "stale" | "action_required"
            );
            if bad {
                failing.push(json!({
                    "name": name,
                    "state": conclusion,
                    "summary": summary,
                }));
            }
            items.push(json!({
                "name": name,
                "status": status,
                "conclusion": conclusion,
                "summary": truncate_str(&summary, 800),
            }));
        }
    }

    let combined = status_body
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if combined == "pending" {
        pending = true;
    }
    if combined == "failure" || combined == "error" {
        if failing.is_empty() {
            failing.push(json!({
                "name": "combined",
                "state": combined,
                "summary": "commit status failed",
            }));
        }
    }

    let state = if !failing.is_empty() {
        "failure"
    } else if pending {
        "pending"
    } else if !saw_any {
        "none"
    } else {
        "success"
    };
    let n_fail = failing.len();
    let summary = match state {
        "failure" => format!("{n_fail} check(s) failed on {owner}/{repo}@{git_ref}"),
        "pending" => format!("Checks still running on {owner}/{repo}@{git_ref}"),
        "none" => format!("No CI checks on {owner}/{repo}@{git_ref}"),
        _ => format!("Checks green on {owner}/{repo}@{git_ref}"),
    };
    ProxyResult::ok_result(
        "github",
        "list_checks",
        summary,
        json!({
            "owner": owner,
            "repo": repo,
            "ref": git_ref,
            "state": state,
            "failing": failing,
            "items": items,
        }),
        Some(if st_runs != 0 { st_runs } else { st_status }),
    )
}

