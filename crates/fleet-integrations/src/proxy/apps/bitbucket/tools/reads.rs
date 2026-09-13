//! Bitbucket read tools.

use serde_json::{json, Value};

use super::super::client::*;
use crate::proxy::types::ProxyResult;

pub(crate) async fn list_repos(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let pagelen = args
        .get("pagelen")
        .or_else(|| args.get("per_page"))
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50);
    let url = format!("https://api.bitbucket.org/2.0/repositories?role=member&pagelen={pagelen}");
    let (status, body) = bb_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_repos", status, &body);
    }
    let items: Vec<Value> = values(&body).iter().map(map_repo).collect();
    let n = items.len();
    ProxyResult::ok_result(
        "bitbucket",
        "list_repos",
        format!("Listed {n} Bitbucket repositories"),
        json!({ "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn list_issues(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (ws, repo) = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("bitbucket", "list_issues", "bad_args", e, None),
    };
    let url = format!("{}?pagelen=20", repo_url(&ws, &repo, "/issues"));
    let (status, body) = bb_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_issues", status, &body);
    }
    let items: Vec<Value> = values(&body).iter().map(map_issue).collect();
    let n = items.len();
    ProxyResult::ok_result(
        "bitbucket",
        "list_issues",
        format!("Listed {n} issues on {ws}/{repo}"),
        json!({ "workspace": ws, "repo": repo, "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn get_issue(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (ws, repo) = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("bitbucket", "get_issue", "bad_args", e, None),
    };
    let id = args
        .get("id")
        .or_else(|| args.get("number"))
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0);
    if id == 0 {
        return ProxyResult::err("bitbucket", "get_issue", "bad_args", "id is required", None);
    }
    let url = repo_url(&ws, &repo, &format!("/issues/{id}"));
    let (status, body) = bb_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("get_issue", status, &body);
    }
    ProxyResult::ok_result(
        "bitbucket",
        "get_issue",
        format!("Fetched issue {id} on {ws}/{repo}"),
        map_issue(&body),
        Some(status),
    )
}

pub(crate) async fn list_prs(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (ws, repo) = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("bitbucket", "list_prs", "bad_args", e, None),
    };
    let state = args.get("state").and_then(|v| v.as_str()).unwrap_or("OPEN");
    let url = format!(
        "{}?state={state}&pagelen=20",
        repo_url(&ws, &repo, "/pullrequests")
    );
    let (status, body) = bb_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_prs", status, &body);
    }
    let items: Vec<Value> = values(&body).iter().map(map_pr).collect();
    let n = items.len();
    ProxyResult::ok_result(
        "bitbucket",
        "list_prs",
        format!("Listed {n} pull requests on {ws}/{repo}"),
        json!({ "workspace": ws, "repo": repo, "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn get_pr(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (ws, repo) = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("bitbucket", "get_pr", "bad_args", e, None),
    };
    let id = args
        .get("id")
        .or_else(|| args.get("number"))
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .unwrap_or(0);
    if id == 0 {
        return ProxyResult::err("bitbucket", "get_pr", "bad_args", "id is required", None);
    }
    let url = repo_url(&ws, &repo, &format!("/pullrequests/{id}"));
    let (status, body) = bb_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("get_pr", status, &body);
    }
    ProxyResult::ok_result(
        "bitbucket",
        "get_pr",
        format!("Fetched PR {id} on {ws}/{repo}"),
        map_pr(&body),
        Some(status),
    )
}

pub(crate) async fn get_file(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (ws, repo) = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("bitbucket", "get_file", "bad_args", e, None),
    };
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("");
    if path.is_empty() {
        return ProxyResult::err(
            "bitbucket",
            "get_file",
            "bad_args",
            "path is required",
            None,
        );
    }
    let rev = args
        .get("ref")
        .or_else(|| args.get("commit"))
        .or_else(|| args.get("branch"))
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD");
    let url = repo_url(&ws, &repo, &format!("/src/{rev}/{path}"));
    let (status, text) = bb_get_text(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("get_file", status, &json!({ "error": text }));
    }
    ProxyResult::ok_result(
        "bitbucket",
        "get_file",
        format!("Read {path} from {ws}/{repo}"),
        json!({ "path": path, "ref": rev, "content": text }),
        Some(status),
    )
}

pub(crate) async fn list_commits(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (ws, repo) = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("bitbucket", "list_commits", "bad_args", e, None),
    };
    let url = format!("{}?pagelen=20", repo_url(&ws, &repo, "/commits"));
    let (status, body) = bb_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_commits", status, &body);
    }
    let items: Vec<Value> = values(&body)
        .iter()
        .map(|c| {
            json!({
                "hash": c.get("hash"),
                "message": c.get("message"),
                "date": c.get("date"),
                "author": c.get("author").and_then(|a| a.get("raw")),
            })
        })
        .collect();
    let n = items.len();
    ProxyResult::ok_result(
        "bitbucket",
        "list_commits",
        format!("Listed {n} commits on {ws}/{repo}"),
        json!({ "workspace": ws, "repo": repo, "count": n, "items": items }),
        Some(status),
    )
}
