//! Gogs HTTP helpers (REST API v1, GitHub-shaped).

use serde_json::{json, Value};

pub(crate) const MAX_COMMIT_FILES: usize = 40;
pub(crate) const MAX_FILE_BYTES: usize = 512 * 1024;

/// Instance root from the connection (`https://gogs.example`), no trailing slash.
pub(crate) fn base_url(config: &Value) -> Result<String, String> {
    let raw = config
        .get("base_url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "gogs base_url is missing on the connection".to_string())?;
    let raw = raw.trim_end_matches('/');
    if !(raw.starts_with("http://") || raw.starts_with("https://")) {
        return Err("gogs base_url must be http(s)".into());
    }
    Ok(raw.to_string())
}

pub(crate) fn api(base: &str, path: &str) -> String {
    format!("{base}/api/v1{path}")
}

/// `owner/repo`, numeric is not used. Accepts `repo`, `project`, or `owner` + `repo`.
pub(crate) fn require_repo(args: &Value) -> Result<String, String> {
    if let (Some(owner), Some(repo)) = (
        args.get("owner").and_then(|v| v.as_str()),
        args.get("repo").and_then(|v| v.as_str()),
    ) {
        let owner = owner.trim();
        let repo = repo.trim().trim_matches('/');
        if !owner.is_empty() && !repo.is_empty() && !repo.contains('/') {
            return Ok(format!("{owner}/{repo}"));
        }
    }
    for key in ["repo", "project", "full_name"] {
        if let Some(full) = args.get(key).and_then(|v| v.as_str()) {
            let full = full.trim().trim_matches('/');
            if full.split('/').filter(|p| !p.is_empty()).count() >= 2 {
                return Ok(full.to_string());
            }
        }
    }
    Err("repo is required (owner/name)".into())
}

pub(crate) fn split_repo(full: &str) -> Result<(String, String), String> {
    let mut parts = full.splitn(2, '/');
    let owner = parts.next().unwrap_or("").trim();
    let repo = parts.next().unwrap_or("").trim();
    if owner.is_empty() || repo.is_empty() {
        return Err(format!("repo must be owner/name, got {full}"));
    }
    Ok((owner.to_string(), repo.to_string()))
}

pub(crate) fn urlencoding(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub(crate) fn path_enc(path: &str) -> String {
    path.split('/')
        .map(urlencoding)
        .collect::<Vec<_>>()
        .join("/")
}

pub(crate) fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}

fn account_name(v: &Value) -> Value {
    let Some(user) = v.get("user") else {
        return Value::Null;
    };
    for key in ["login", "username"] {
        if let Some(name) = user.get(key).and_then(|n| n.as_str()).filter(|s| !s.is_empty()) {
            return Value::String(name.to_string());
        }
    }
    Value::Null
}

fn repo_full_name(p: &Value) -> Value {
    if let Some(full) = p.get("full_name").and_then(|v| v.as_str()).filter(|s| s.contains('/')) {
        return Value::String(full.to_string());
    }
    let owner = p.get("owner").and_then(|o| {
        o.get("username")
            .or_else(|| o.get("login"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    });
    let name = p.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty());
    match (owner, name) {
        (Some(owner), Some(name)) => Value::String(format!("{owner}/{name}")),
        _ => Value::Null,
    }
}

pub(crate) fn map_repo(p: &Value) -> Value {
    let full = repo_full_name(p);
    json!({
        "id": p.get("id"),
        "full_name": full.clone(),
        "path_with_namespace": full,
        "name": p.get("name"),
        "description": p.get("description"),
        "html_url": p.get("html_url"),
        "web_url": p.get("html_url"),
        "default_branch": p.get("default_branch"),
        "private": p.get("private"),
        "updated_at": p.get("updated_at"),
    })
}

pub(crate) fn map_issue(i: &Value) -> Value {
    let number = i.get("number").or_else(|| i.get("index"));
    let who = account_name(i);
    json!({
        "number": number,
        "iid": number,
        "id": i.get("id"),
        "title": i.get("title"),
        "state": i.get("state"),
        "html_url": i.get("html_url"),
        "web_url": i.get("html_url"),
        "body": truncate_str(i.get("body").and_then(|v| v.as_str()).unwrap_or(""), 2000),
        "description": truncate_str(i.get("body").and_then(|v| v.as_str()).unwrap_or(""), 2000),
        "user": who.clone(),
        "author": who,
        "created_at": i.get("created_at"),
        "updated_at": i.get("updated_at"),
    })
}

pub(crate) fn map_pr(m: &Value) -> Value {
    let number = m.get("number").or_else(|| m.get("index"));
    let who = account_name(m);
    json!({
        "number": number,
        "iid": number,
        "id": m.get("id"),
        "title": m.get("title"),
        "state": m.get("state"),
        "html_url": m.get("html_url"),
        "web_url": m.get("html_url"),
        "body": truncate_str(m.get("body").and_then(|v| v.as_str()).unwrap_or(""), 2000),
        "description": truncate_str(m.get("body").and_then(|v| v.as_str()).unwrap_or(""), 2000),
        "user": who.clone(),
        "author": who,
        "draft": m.get("draft"),
        "head": m.get("head").and_then(|h| h.get("ref")).or_else(|| m.get("head").and_then(|h| h.get("label"))),
        "base": m.get("base").and_then(|h| h.get("ref")).or_else(|| m.get("base").and_then(|h| h.get("label"))),
        "source_branch": m.get("head").and_then(|h| h.get("ref")),
        "target_branch": m.get("base").and_then(|h| h.get("ref")),
        "merged": m.get("merged"),
        "created_at": m.get("created_at"),
        "updated_at": m.get("updated_at"),
    })
}

fn headers(req: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    req.header("Authorization", format!("token {token}"))
        .header("Accept", "application/json")
}

pub(crate) async fn gogs_get(http: &reqwest::Client, token: &str, url: &str) -> (u16, Value) {
    match headers(http.get(url), token).send().await {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.json::<Value>().await.unwrap_or(json!({}));
            (status, body)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

pub(crate) async fn gogs_send(
    http: &reqwest::Client,
    token: &str,
    method: reqwest::Method,
    url: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let mut req = headers(http.request(method, url), token);
    if let Some(body) = body {
        req = req.json(body);
    }
    match req.send().await {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.json::<Value>().await.unwrap_or(json!({}));
            (status, body)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

pub(crate) fn repo_api(base: &str, full: &str, suffix: &str) -> Result<String, String> {
    let (owner, repo) = split_repo(full)?;
    Ok(api(
        base,
        &format!(
            "/repos/{}/{}{}",
            urlencoding(&owner),
            urlencoding(&repo),
            suffix
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_rejects_missing_and_relative() {
        assert!(base_url(&json!({})).is_err());
        assert!(base_url(&json!({"base_url": "gogs.local"})).is_err());
        assert_eq!(
            base_url(&json!({"base_url": "http://localhost:3034/"})).unwrap(),
            "http://localhost:3034"
        );
    }

    #[test]
    fn repo_from_owner_or_full_name() {
        assert_eq!(
            require_repo(&json!({"owner": "worms", "repo": "game"})).unwrap(),
            "worms/game"
        );
        assert_eq!(
            require_repo(&json!({"project": "worms/game"})).unwrap(),
            "worms/game"
        );
        assert!(require_repo(&json!({"repo": "game"})).is_err());
    }

    #[test]
    fn repo_and_issue_use_gogs_username_when_login_is_absent() {
        let repo = map_repo(&json!({
            "name": "game",
            "owner": {"username": "worms"}
        }));
        assert_eq!(repo["full_name"], "worms/game");
        let issue = map_issue(&json!({
            "number": 4,
            "title": "Add a worm",
            "user": {"username": "fleet"}
        }));
        assert_eq!(issue["number"], 4);
        assert_eq!(issue["user"], "fleet");
    }
}
