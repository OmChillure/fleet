//! GitHub HTTP client helpers (control plane only).

use serde_json::{json, Value};

use crate::proxy::types::ProxyResult;

pub(crate) const MAX_COMMIT_FILES: usize = 40;
pub(crate) const MAX_FILE_BYTES: usize = 512 * 1024;

pub(crate) fn require_owner_repo(args: &Value) -> Result<(String, String), String> {
    // Accept owner+repo or "repo": "owner/name"
    if let (Some(owner), Some(repo)) = (
        args.get("owner").and_then(|v| v.as_str()),
        args.get("repo").and_then(|v| v.as_str()),
    ) {
        let owner = owner.trim();
        let repo = repo.trim();
        if repo.contains('/') {
            let mut parts = repo.splitn(2, '/');
            let o = parts.next().unwrap_or("").trim();
            let r = parts.next().unwrap_or("").trim();
            if !o.is_empty() && !r.is_empty() {
                return Ok((o.to_string(), r.to_string()));
            }
        }
        if !owner.is_empty() && !repo.is_empty() {
            return Ok((owner.to_string(), repo.to_string()));
        }
    }
    if let Some(full) = args.get("repo").and_then(|v| v.as_str()) {
        let mut parts = full.splitn(2, '/');
        let o = parts.next().unwrap_or("").trim();
        let r = parts.next().unwrap_or("").trim();
        if !o.is_empty() && !r.is_empty() {
            return Ok((o.to_string(), r.to_string()));
        }
    }
    if let Some(full) = args.get("full_name").and_then(|v| v.as_str()) {
        let mut parts = full.splitn(2, '/');
        let o = parts.next().unwrap_or("").trim();
        let r = parts.next().unwrap_or("").trim();
        if !o.is_empty() && !r.is_empty() {
            return Ok((o.to_string(), r.to_string()));
        }
    }
    Err("owner and repo required (or repo as owner/name)".into())
}

pub(crate) fn map_issue(i: &Value) -> Value {
    let labels: Vec<Value> = i
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|l| l.get("name").cloned()).collect())
        .unwrap_or_default();
    json!({
        "number": i.get("number"),
        "title": i.get("title"),
        "state": i.get("state"),
        "html_url": i.get("html_url"),
        "body": truncate_str(i.get("body").and_then(|v| v.as_str()).unwrap_or(""), 2000),
        "user": i.get("user").and_then(|u| u.get("login")),
        "labels": labels,
        "created_at": i.get("created_at"),
        "updated_at": i.get("updated_at"),
    })
}

pub(crate) fn map_pr(p: &Value) -> Value {
    json!({
        "number": p.get("number"),
        "title": p.get("title"),
        "state": p.get("state"),
        "html_url": p.get("html_url"),
        "body": truncate_str(p.get("body").and_then(|v| v.as_str()).unwrap_or(""), 2000),
        "user": p.get("user").and_then(|u| u.get("login")),
        "draft": p.get("draft"),
        "merged_at": p.get("merged_at"),
        "base": p.get("base").and_then(|b| b.get("ref")),
        "head": p.get("head").and_then(|h| h.get("ref")),
        "created_at": p.get("created_at"),
        "updated_at": p.get("updated_at"),
    })
}

pub(crate) fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}

// ── writes (control-plane; OAuth token never returned) ─────────────────────

/// Create or update a single file via Contents API.
pub(crate) fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub(crate) fn path_enc(path: &str) -> String {
    path.split('/')
        .map(urlencoding)
        .collect::<Vec<_>>()
        .join("/")
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

pub(crate) fn github_api_origin() -> String {
    #[cfg(test)]
    if let Some(base) = TEST_GITHUB_API_BASE.with(|c| c.borrow().clone()) {
        return base;
    }
    "https://api.github.com".into()
}

#[cfg(test)]
thread_local! {
    pub(crate) static TEST_GITHUB_API_BASE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

pub(crate) fn github_headers(req: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    req.header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "agent-fleet-proxy")
        .header("X-GitHub-Api-Version", "2022-11-28")
}

pub(crate) async fn github_get(http: &reqwest::Client, token: &str, url: &str) -> (u16, Value) {
    match github_headers(http.get(url), token).send().await {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.json::<Value>().await.unwrap_or(json!({}));
            (status, body)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

pub(crate) async fn github_post(
    http: &reqwest::Client,
    token: &str,
    url: &str,
    body: &Value,
) -> (u16, Value) {
    match github_headers(http.post(url).json(body), token)
        .send()
        .await
    {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.json::<Value>().await.unwrap_or(json!({}));
            (status, body)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

pub(crate) async fn github_put(
    http: &reqwest::Client,
    token: &str,
    url: &str,
    body: &Value,
) -> (u16, Value) {
    match github_headers(http.put(url).json(body), token).send().await {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.json::<Value>().await.unwrap_or(json!({}));
            (status, body)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

pub(crate) async fn github_patch(
    http: &reqwest::Client,
    token: &str,
    url: &str,
    body: &Value,
) -> (u16, Value) {
    match github_headers(http.patch(url).json(body), token)
        .send()
        .await
    {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.json::<Value>().await.unwrap_or(json!({}));
            (status, body)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

pub(crate) fn upstream_err(op: &str, status: u16, body: &Value) -> ProxyResult {
    let msg = body
        .get("message")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("error").and_then(|v| v.as_str()))
        .unwrap_or("upstream error");
    let code = if status == 401 || status == 403 {
        "token_expired"
    } else {
        "upstream"
    };
    ProxyResult::err(
        "github",
        op,
        code,
        format!("GitHub API {status}: {msg}"),
        Some(status),
    )
}
