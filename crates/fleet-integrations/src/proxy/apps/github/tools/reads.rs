//! GitHub read tools.

use serde_json::{Value, json};

use super::super::client::*;
use crate::proxy::types::ProxyResult;

pub(crate) async fn list_repos(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
    config: &Value,
) -> ProxyResult {
    let per_page = args
        .get("per_page")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50);
    let origin = github_api_origin();
    // GitHub App installation tokens 403 on GET /user/repos. The install's
    // own repository list is the call that matches "app is allowed".
    let installation = config
        .get("installation_id")
        .and_then(|v| v.as_i64())
        .filter(|id| *id > 0);
    let url = if installation.is_some() {
        format!("{origin}/installation/repositories?per_page={per_page}")
    } else {
        format!("{origin}/user/repos?per_page={per_page}&sort=updated")
    };
    let (status, body) = github_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("list_repos", status, &body);
    }
    let repos = if installation.is_some() {
        body.get("repositories").and_then(|v| v.as_array())
    } else {
        body.as_array()
    };
    let items: Vec<Value> = repos
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
    let state = args.get("state").and_then(|v| v.as_str()).unwrap_or("open");
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
            );
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
    let state = args.get("state").and_then(|v| v.as_str()).unwrap_or("open");
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
            return ProxyResult::err("github", "get_pr", "bad_args", "number is required", None);
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
    if let Some(r) = args
        .get("ref")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        url.push_str(&format!("?ref={}", urlencoding(r)));
    }
    let (status, body) = github_get(http, token, &url).await;
    if !(200..300).contains(&status) {
        return upstream_err("get_file", status, &body);
    }
    if body.get("type").and_then(|v| v.as_str()) == Some("dir") || body.as_array().is_some() {
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

pub(crate) async fn list_commits(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (owner, repo) = match require_owner_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("github", "list_commits", "bad_args", e, None),
    };
    let per_page = args
        .get("per_page")
        .and_then(|v| v.as_u64())
        .unwrap_or(20)
        .clamp(1, 50);
    let mut url =
        format!("https://api.github.com/repos/{owner}/{repo}/commits?per_page={per_page}");
    if let Some(sha) = args
        .get("sha")
        .or_else(|| args.get("branch"))
        .and_then(|v| v.as_str())
    {
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

#[cfg(test)]
mod list_repos_tests {
    use super::*;
    use crate::proxy::dispatch;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    const LEAK: &str = "gho_LIST_REPOS_MUST_NOT_ECHO_c0ffee";

    struct OriginGuard;
    impl Drop for OriginGuard {
        fn drop(&mut self) {
            TEST_GITHUB_API_BASE.with(|c| *c.borrow_mut() = None);
        }
    }

    struct GitHubListSink {
        hits: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        origin: String,
        last_request: Arc<Mutex<Option<String>>>,
    }

    impl Drop for GitHubListSink {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
        }
    }

    impl GitHubListSink {
        fn bind(status: u16, body: Value) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind github list_repos sink");
            listener.set_nonblocking(true).expect("nonblocking sink");
            let addr = listener.local_addr().expect("sink addr");
            let hits = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let last_request = Arc::new(Mutex::new(None));
            let hits_t = hits.clone();
            let stop_t = stop.clone();
            let last_request_t = last_request.clone();
            let payload = body.to_string();
            thread::spawn(move || {
                while !stop_t.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            hits_t.fetch_add(1, Ordering::SeqCst);
                            let _ = stream.set_nonblocking(false);
                            let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                            let mut buf = Vec::new();
                            let mut tmp = [0u8; 4096];
                            loop {
                                match stream.read(&mut tmp) {
                                    Ok(0) => break,
                                    Ok(n) => {
                                        buf.extend_from_slice(&tmp[..n]);
                                        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                            break;
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                            if let Some(line) = std::str::from_utf8(&buf)
                                .ok()
                                .and_then(|s| s.lines().next())
                                .map(str::to_string)
                            {
                                *last_request_t.lock().expect("last_request") = Some(line);
                            }
                            let resp = format!(
                                "HTTP/1.1 {status} ok\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                                payload.len()
                            );
                            let _ = stream.write_all(resp.as_bytes());
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                hits,
                stop,
                origin: format!("http://{addr}"),
                last_request,
            }
        }

        fn hits(&self) -> usize {
            self.hits.load(Ordering::SeqCst)
        }

        fn last_request(&self) -> Option<String> {
            self.last_request.lock().expect("last_request").clone()
        }
    }

    fn mapped_repo() -> Value {
        json!({
            "full_name": "acme/app",
            "private": false,
            "html_url": "https://github.com/acme/app",
            "description": "demo",
            "default_branch": "main",
            "language": "Rust",
            "updated_at": "2024-01-01T00:00:00Z",
            "access_token": LEAK,
        })
    }

    async fn dispatch_list_repos(origin: &str, args: Value) -> ProxyResult {
        dispatch_list_repos_with(origin, args, json!({})).await
    }

    async fn dispatch_list_repos_with(origin: &str, args: Value, config: Value) -> ProxyResult {
        let _guard = OriginGuard;
        TEST_GITHUB_API_BASE.with(|c| *c.borrow_mut() = Some(origin.to_string()));
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("http");
        dispatch("github", &http, LEAK, "list_repos", &args, &config).await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn github_list_repos_maps_user_repos_json() {
        let sink = GitHubListSink::bind(200, json!([mapped_repo()]));
        let result = dispatch_list_repos(&sink.origin, json!({})).await;
        assert!(result.ok, "list_repos 200 must be ok: {result:?}");
        assert_eq!(result.provider, "github", "{result:?}");
        assert_eq!(result.operation, "list_repos", "{result:?}");
        assert_eq!(
            result.data["count"],
            json!(1),
            "count must match items: {result:?}"
        );
        let item = &result.data["items"][0];
        assert_eq!(
            item["full_name"],
            json!("acme/app"),
            "must map full_name: {result:?}"
        );
        assert_eq!(
            item["private"],
            json!(false),
            "must map private: {result:?}"
        );
        assert_eq!(
            item["html_url"],
            json!("https://github.com/acme/app"),
            "must map html_url: {result:?}"
        );
        assert_eq!(
            item["description"],
            json!("demo"),
            "must map description: {result:?}"
        );
        assert_eq!(
            item["default_branch"],
            json!("main"),
            "must map default_branch: {result:?}"
        );
        assert_eq!(
            item["language"],
            json!("Rust"),
            "must map language: {result:?}"
        );
        assert_eq!(
            item["updated_at"],
            json!("2024-01-01T00:00:00Z"),
            "must map updated_at: {result:?}"
        );
        let dump = serde_json::to_string(&result).expect("serialize");
        assert!(
            !dump.contains(LEAK) && !dump.to_ascii_lowercase().contains("bearer"),
            "list_repos must not echo bearer/token: {dump}"
        );
        assert!(sink.hits() > 0, "list_repos must hit the sink: {result:?}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn github_list_repos_upstream_401_is_token_expired() {
        let sink = GitHubListSink::bind(
            401,
            json!({ "message": "Bad credentials", "access_token": LEAK }),
        );
        let result = dispatch_list_repos(&sink.origin, json!({})).await;
        assert!(!result.ok, "401 must be ok:false: {result:?}");
        assert_eq!(
            result.error_code.as_deref(),
            Some("token_expired"),
            "GitHub 401 must map to token_expired: {result:?}"
        );
        assert_eq!(
            result.provider_status,
            Some(401),
            "provider_status must be 401: {result:?}"
        );
        assert_eq!(result.provider, "github", "{result:?}");
        assert_eq!(result.operation, "list_repos", "{result:?}");
        let dump = serde_json::to_string(&result).expect("serialize");
        assert!(
            !dump.contains(LEAK),
            "token_expired must not echo token: {dump}"
        );
        assert!(sink.hits() > 0, "401 must hit the sink: {result:?}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn github_list_repos_403_does_not_expire_the_connection() {
        let sink = GitHubListSink::bind(
            403,
            json!({ "message": "Resource not accessible by integration" }),
        );
        let result = dispatch_list_repos(&sink.origin, json!({})).await;
        assert!(!result.ok, "403 must be ok:false: {result:?}");
        assert_eq!(
            result.error_code.as_deref(),
            Some("upstream"),
            "a permissions 403 must not be token_expired: {result:?}"
        );
        assert_eq!(result.provider_status, Some(403), "{result:?}");
        assert!(
            result
                .summary
                .contains("Resource not accessible by integration"),
            "{result:?}"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn github_list_repos_installation_uses_installation_repositories() {
        let sink = GitHubListSink::bind(
            200,
            json!({ "total_count": 1, "repositories": [mapped_repo()] }),
        );
        let result = dispatch_list_repos_with(
            &sink.origin,
            json!({}),
            json!({ "installation_id": 161434524 }),
        )
        .await;
        assert!(result.ok, "installation list must be ok: {result:?}");
        assert_eq!(result.data["count"], json!(1), "{result:?}");
        assert_eq!(
            result.data["items"][0]["full_name"],
            json!("acme/app"),
            "{result:?}"
        );
        let req = sink.last_request().unwrap_or_default();
        assert!(
            req.contains("/installation/repositories"),
            "installation id must list installation repos: {req}"
        );
        assert!(
            !req.contains("/user/repos"),
            "installation list must not call /user/repos: {req}"
        );
        let dump = serde_json::to_string(&result).expect("serialize");
        assert!(!dump.contains(LEAK), "must not echo token: {dump}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn github_list_repos_per_page_clamped_to_50() {
        let sink = GitHubListSink::bind(200, json!([mapped_repo()]));
        let result = dispatch_list_repos(&sink.origin, json!({ "per_page": 999 })).await;
        assert!(result.ok, "clamped list_repos 200 must be ok: {result:?}");
        let req = sink
            .last_request()
            .unwrap_or_else(|| panic!("GET line: {result:?}"));
        assert!(
            req.contains("/user/repos") && req.contains("per_page=50"),
            "per_page=999 must clamp to 50: {req}"
        );
        assert!(
            !req.contains("per_page=999"),
            "unclamped per_page must not be sent: {req}"
        );
        assert!(sink.hits() > 0, "clamp test must hit the sink: {result:?}");
    }
}
