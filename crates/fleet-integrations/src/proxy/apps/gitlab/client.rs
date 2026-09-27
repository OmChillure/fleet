//! GitLab HTTP helpers (gitlab.com REST v4).

use serde_json::{json, Value};

use crate::proxy::types::ProxyResult;

pub(crate) const MAX_COMMIT_FILES: usize = 40;
pub(crate) const MAX_FILE_BYTES: usize = 512 * 1024;
const API: &str = "https://gitlab.com/api/v4";

/// Accept `project` (`group/name` or numeric id) or GitHub-style `owner` + `repo`.
pub(crate) fn require_project(args: &Value) -> Result<String, String> {
    if let Some(p) = args
        .get("project")
        .or_else(|| args.get("project_id"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(p.to_string());
    }
    if let Some(id) = args
        .get("project")
        .or_else(|| args.get("project_id"))
        .and_then(|v| v.as_u64())
    {
        return Ok(id.to_string());
    }
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
                return Ok(format!("{o}/{r}"));
            }
        }
        if !owner.is_empty() && !repo.is_empty() {
            return Ok(format!("{owner}/{repo}"));
        }
    }
    if let Some(full) = args
        .get("repo")
        .or_else(|| args.get("full_name"))
        .and_then(|v| v.as_str())
    {
        let full = full.trim();
        if !full.is_empty() {
            return Ok(full.to_string());
        }
    }
    Err("project is required (group/name, numeric id, or owner + repo)".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn project_from_path_owner_repo_or_id() {
        assert_eq!(
            require_project(&json!({ "project": "acme/app" })).unwrap(),
            "acme/app"
        );
        assert_eq!(
            require_project(&json!({ "owner": "acme", "repo": "app" })).unwrap(),
            "acme/app"
        );
        assert_eq!(require_project(&json!({ "project": 42 })).unwrap(), "42");
        assert!(require_project(&json!({})).is_err());
    }
}

pub(crate) fn project_enc(project: &str) -> String {
    urlencoding(project)
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
    urlencoding(path)
}

pub(crate) fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{t}…")
    }
}

pub(crate) fn map_project(p: &Value) -> Value {
    json!({
        "id": p.get("id"),
        "path_with_namespace": p.get("path_with_namespace"),
        "name": p.get("name"),
        "description": p.get("description"),
        "web_url": p.get("web_url"),
        "default_branch": p.get("default_branch"),
        "visibility": p.get("visibility"),
        "last_activity_at": p.get("last_activity_at"),
    })
}

pub(crate) fn map_issue(i: &Value) -> Value {
    let labels = i.get("labels").cloned().unwrap_or(json!([]));
    json!({
        "iid": i.get("iid"),
        "id": i.get("id"),
        "title": i.get("title"),
        "state": i.get("state"),
        "web_url": i.get("web_url"),
        "description": truncate_str(i.get("description").and_then(|v| v.as_str()).unwrap_or(""), 2000),
        "author": i.get("author").and_then(|a| a.get("username")),
        "labels": labels,
        "created_at": i.get("created_at"),
        "updated_at": i.get("updated_at"),
    })
}

pub(crate) fn map_mr(m: &Value) -> Value {
    json!({
        "iid": m.get("iid"),
        "id": m.get("id"),
        "title": m.get("title"),
        "state": m.get("state"),
        "web_url": m.get("web_url"),
        "description": truncate_str(m.get("description").and_then(|v| v.as_str()).unwrap_or(""), 2000),
        "author": m.get("author").and_then(|a| a.get("username")),
        "draft": m.get("draft").or_else(|| m.get("work_in_progress")),
        "source_branch": m.get("source_branch"),
        "target_branch": m.get("target_branch"),
        "merged_at": m.get("merged_at"),
        "created_at": m.get("created_at"),
        "updated_at": m.get("updated_at"),
    })
}

fn gitlab_headers(req: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    req.header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
}

pub(crate) async fn gitlab_get(http: &reqwest::Client, token: &str, url: &str) -> (u16, Value) {
    match gitlab_headers(http.get(url), token).send().await {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.json::<Value>().await.unwrap_or(json!({}));
            (status, body)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

pub(crate) async fn gitlab_post(
    http: &reqwest::Client,
    token: &str,
    url: &str,
    body: &Value,
) -> (u16, Value) {
    match gitlab_headers(http.post(url).json(body), token)
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

pub(crate) async fn gitlab_put(
    http: &reqwest::Client,
    token: &str,
    url: &str,
    body: &Value,
) -> (u16, Value) {
    match gitlab_headers(http.put(url).json(body), token).send().await {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.json::<Value>().await.unwrap_or(json!({}));
            (status, body)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

pub(crate) fn project_url(project: &str, suffix: &str) -> String {
    format!("{API}/projects/{}{suffix}", project_enc(project))
}

pub(crate) fn upstream_err(op: &str, status: u16, body: &Value) -> ProxyResult {
    let msg = body
        .get("message")
        .and_then(|v| {
            v.as_str()
                .map(|s| s.to_string())
                .or_else(|| serde_json::to_string(v).ok())
        })
        .or_else(|| {
            body.get("error")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "upstream error".into());
    let code = if status == 401 {
        "token_expired"
    } else {
        "upstream"
    };
    ProxyResult::err(
        "gitlab",
        op,
        code,
        format!("GitLab API {status}: {msg}"),
        Some(status),
    )
}
