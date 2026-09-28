//! Gitea read tools.

use serde_json::{json, Value};

use super::super::client::*;
use crate::proxy::types::ProxyResult;

fn bad(op: &str, e: String) -> ProxyResult {
    ProxyResult::err("gitea", op, "bad_args", e, None)
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

pub(crate) async fn list_repos(
    http: &reqwest::Client,
    token: &str,
    base: &str,
    args: &Value,
) -> ProxyResult {
    let limit = args
        .get("per_page")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50);
    let url = format!("{}?limit={limit}", api(base, "/user/repos"));
    let (status, body) = gitea_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream("list_repos", status, &body);
    }
    let items: Vec<Value> = body
        .as_array()
        .map(|arr| arr.iter().map(map_repo).collect())
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "gitea",
        "list_repos",
        format!("Listed {n} Gitea repositories"),
        json!({ "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn list_issues(
    http: &reqwest::Client,
    token: &str,
    base: &str,
    args: &Value,
) -> ProxyResult {
    let repo = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return bad("list_issues", e),
    };
    let state = args.get("state").and_then(|v| v.as_str()).unwrap_or("open");
    let limit = args
        .get("per_page")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50);
    let url = match repo_api(base, &repo, "/issues") {
        Ok(u) => format!("{u}?state={}&type=issues&limit={limit}", urlencoding(state)),
        Err(e) => return bad("list_issues", e),
    };
    let (status, body) = gitea_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream("list_issues", status, &body);
    }
    let items: Vec<Value> = body
        .as_array()
        .map(|arr| arr.iter().map(map_issue).collect())
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "gitea",
        "list_issues",
        format!("Listed {n} issues on {repo}"),
        json!({ "repo": repo, "count": n, "items": items }),
        Some(status),
    )
}

fn number(args: &Value) -> Result<u64, String> {
    args.get("number")
        .or_else(|| args.get("iid"))
        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .ok_or_else(|| "number is required".into())
}

pub(crate) async fn get_issue(
    http: &reqwest::Client,
    token: &str,
    base: &str,
    args: &Value,
) -> ProxyResult {
    let repo = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return bad("get_issue", e),
    };
    let n = match number(args) {
        Ok(n) => n,
        Err(e) => return bad("get_issue", e),
    };
    let url = match repo_api(base, &repo, &format!("/issues/{n}")) {
        Ok(u) => u,
        Err(e) => return bad("get_issue", e),
    };
    let (status, body) = gitea_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream("get_issue", status, &body);
    }
    ProxyResult::ok_result(
        "gitea",
        "get_issue",
        format!("Fetched issue #{n} on {repo}"),
        map_issue(&body),
        Some(status),
    )
}

pub(crate) async fn list_mrs(
    http: &reqwest::Client,
    token: &str,
    base: &str,
    args: &Value,
) -> ProxyResult {
    let repo = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return bad("list_mrs", e),
    };
    let state = args.get("state").and_then(|v| v.as_str()).unwrap_or("open");
    let url = match repo_api(base, &repo, "/pulls") {
        Ok(u) => format!("{u}?state={}", urlencoding(state)),
        Err(e) => return bad("list_mrs", e),
    };
    let (status, body) = gitea_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream("list_mrs", status, &body);
    }
    let items: Vec<Value> = body
        .as_array()
        .map(|arr| arr.iter().map(map_pr).collect())
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "gitea",
        "list_mrs",
        format!("Listed {n} pull requests on {repo}"),
        json!({ "repo": repo, "count": n, "items": items }),
        Some(status),
    )
}

pub(crate) async fn get_mr(
    http: &reqwest::Client,
    token: &str,
    base: &str,
    args: &Value,
) -> ProxyResult {
    let repo = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return bad("get_mr", e),
    };
    let n = match number(args) {
        Ok(n) => n,
        Err(e) => return bad("get_mr", e),
    };
    let url = match repo_api(base, &repo, &format!("/pulls/{n}")) {
        Ok(u) => u,
        Err(e) => return bad("get_mr", e),
    };
    let (status, body) = gitea_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream("get_mr", status, &body);
    }
    ProxyResult::ok_result(
        "gitea",
        "get_mr",
        format!("Fetched pull request #{n} on {repo}"),
        map_pr(&body),
        Some(status),
    )
}

pub(crate) async fn get_file(
    http: &reqwest::Client,
    token: &str,
    base: &str,
    args: &Value,
) -> ProxyResult {
    let repo = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return bad("get_file", e),
    };
    let path = match args.get("path").and_then(|v| v.as_str()).map(str::trim) {
        Some(p) if !p.is_empty() => p,
        _ => return bad("get_file", "path is required".into()),
    };
    let mut url = match repo_api(base, &repo, &format!("/contents/{}", path_enc(path))) {
        Ok(u) => u,
        Err(e) => return bad("get_file", e),
    };
    if let Some(r) = args.get("ref").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        url.push_str(&format!("?ref={}", urlencoding(r)));
    }
    let (status, body) = gitea_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream("get_file", status, &body);
    }
    let content_b64 = body.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let text = decode_b64(content_b64);
    ProxyResult::ok_result(
        "gitea",
        "get_file",
        format!("Read {path} on {repo}"),
        json!({
            "repo": repo,
            "path": path,
            "sha": body.get("sha"),
            "encoding": "utf-8",
            "content": truncate_str(&text, 8000),
        }),
        Some(status),
    )
}

pub(crate) async fn list_commits(
    http: &reqwest::Client,
    token: &str,
    base: &str,
    args: &Value,
) -> ProxyResult {
    let repo = match require_repo(args) {
        Ok(v) => v,
        Err(e) => return bad("list_commits", e),
    };
    let mut url = match repo_api(base, &repo, "/commits") {
        Ok(u) => u,
        Err(e) => return bad("list_commits", e),
    };
    url.push_str("?limit=20");
    if let Some(sha) = args.get("sha").or_else(|| args.get("ref")).and_then(|v| v.as_str()) {
        url.push_str(&format!("&sha={}", urlencoding(sha)));
    }
    let (status, body) = gitea_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream("list_commits", status, &body);
    }
    let items: Vec<Value> = body
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|c| {
                    json!({
                        "sha": c.get("sha"),
                        "html_url": c.get("html_url"),
                        "message": truncate_str(
                            c.get("commit").and_then(|m| m.get("message")).and_then(|v| v.as_str()).unwrap_or(""),
                            240
                        ),
                        "created": c.get("created"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let n = items.len();
    ProxyResult::ok_result(
        "gitea",
        "list_commits",
        format!("Listed {n} commits on {repo}"),
        json!({ "repo": repo, "count": n, "items": items }),
        Some(status),
    )
}

fn decode_b64(raw: &str) -> String {
    let cleaned: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = base64_decode(&cleaned);
    String::from_utf8_lossy(&bytes).into_owned()
}

fn base64_decode(s: &str) -> Vec<u8> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 < bytes.len() {
        let (Some(a), Some(b)) = (val(bytes[i]), val(bytes[i + 1])) else {
            break;
        };
        let c = if bytes[i + 2] == b'=' { None } else { val(bytes[i + 2]) };
        let d = if bytes[i + 3] == b'=' { None } else { val(bytes[i + 3]) };
        out.push((a << 2) | (b >> 4));
        if let Some(c) = c {
            out.push((b << 4) | (c >> 2));
            if let Some(d) = d {
                out.push((c << 6) | d);
            }
        }
        i += 4;
    }
    out
}
