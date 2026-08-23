//! Bitbucket Cloud REST 2.0 helpers.

use serde_json::{json, Value};

use crate::proxy::types::ProxyResult;

const API: &str = "https://api.bitbucket.org/2.0";

pub(crate) fn require_repo(args: &Value) -> Result<(String, String), String> {
    if let (Some(ws), Some(slug)) = (
        args.get("workspace")
            .or_else(|| args.get("owner"))
            .and_then(|v| v.as_str()),
        args.get("repo_slug")
            .or_else(|| args.get("repo"))
            .and_then(|v| v.as_str()),
    ) {
        let ws = ws.trim();
        let slug = slug.trim().trim_end_matches('/').to_string();
        if slug.contains('/') {
            let mut p = slug.splitn(2, '/');
            let a = p.next().unwrap_or("").trim();
            let b = p.next().unwrap_or("").trim();
            if !a.is_empty() && !b.is_empty() {
                return Ok((a.to_string(), b.to_string()));
            }
        }
        if !ws.is_empty() && !slug.is_empty() {
            return Ok((ws.to_string(), slug));
        }
    }
    if let Some(full) = args
        .get("full_name")
        .or_else(|| args.get("repo"))
        .and_then(|v| v.as_str())
    {
        if let Some((a, b)) = full.trim().split_once('/') {
            if !a.is_empty() && !b.is_empty() {
                return Ok((a.to_string(), b.to_string()));
            }
        }
    }
    Err("workspace + repo (or owner/repo) is required".into())
}

pub(crate) fn repo_url(workspace: &str, repo: &str, suffix: &str) -> String {
    format!("{API}/repositories/{workspace}/{repo}{suffix}")
}

pub(crate) fn map_repo(r: &Value) -> Value {
    json!({
        "full_name": r.get("full_name"),
        "name": r.get("name"),
        "description": r.get("description"),
        "is_private": r.get("is_private"),
        "mainbranch": r.get("mainbranch").and_then(|b| b.get("name")),
        "updated_on": r.get("updated_on"),
        "links": r.get("links").and_then(|l| l.get("html")).and_then(|h| h.get("href")),
    })
}

pub(crate) fn map_pr(p: &Value) -> Value {
    json!({
        "id": p.get("id"),
        "title": p.get("title"),
        "state": p.get("state"),
        "source": p.get("source").and_then(|s| s.get("branch")).and_then(|b| b.get("name")),
        "destination": p.get("destination").and_then(|s| s.get("branch")).and_then(|b| b.get("name")),
        "author": p.get("author").and_then(|a| a.get("display_name")),
        "links": p.get("links").and_then(|l| l.get("html")).and_then(|h| h.get("href")),
        "updated_on": p.get("updated_on"),
    })
}

pub(crate) fn map_issue(i: &Value) -> Value {
    json!({
        "id": i.get("id"),
        "title": i.get("title"),
        "state": i.get("state").and_then(|s| s.get("name")),
        "kind": i.get("kind"),
        "priority": i.get("priority"),
        "content": i.get("content").and_then(|c| c.get("raw")),
        "links": i.get("links").and_then(|l| l.get("html")).and_then(|h| h.get("href")),
        "updated_on": i.get("updated_on"),
    })
}

fn headers(req: reqwest::RequestBuilder, token: &str) -> reqwest::RequestBuilder {
    req.header("Authorization", format!("Bearer {token}"))
        .header("Accept", "application/json")
}

pub(crate) async fn bb_get(http: &reqwest::Client, token: &str, url: &str) -> (u16, Value) {
    match headers(http.get(url), token).send().await {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.json::<Value>().await.unwrap_or(json!({}));
            (status, body)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

pub(crate) async fn bb_get_text(http: &reqwest::Client, token: &str, url: &str) -> (u16, String) {
    match headers(http.get(url), token).send().await {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.text().await.unwrap_or_default();
            (status, body)
        }
        Err(e) => (0, e.to_string()),
    }
}

pub(crate) async fn bb_put(
    http: &reqwest::Client,
    token: &str,
    url: &str,
    body: &Value,
) -> (u16, Value) {
    match headers(http.put(url).json(body), token).send().await {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.json::<Value>().await.unwrap_or(json!({}));
            (status, body)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

pub(crate) async fn bb_post(
    http: &reqwest::Client,
    token: &str,
    url: &str,
    body: &Value,
) -> (u16, Value) {
    match headers(http.post(url).json(body), token).send().await {
        Ok(res) => {
            let status = res.status().as_u16();
            let body = res.json::<Value>().await.unwrap_or(json!({}));
            (status, body)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

pub(crate) fn values(body: &Value) -> Vec<Value> {
    body.get("values")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

pub(crate) fn upstream_err(op: &str, status: u16, body: &Value) -> ProxyResult {
    let msg = body
        .get("error")
        .and_then(|e| e.get("message").or_else(|| e.get("detail")))
        .and_then(|v| v.as_str())
        .or_else(|| body.get("error").and_then(|v| v.as_str()))
        .unwrap_or("upstream error");
    let code = if status == 401 || status == 403 {
        "token_expired"
    } else {
        "upstream"
    };
    ProxyResult::err("bitbucket", op, code, format!("{msg} ({status})"), Some(status))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_from_owner_repo_or_full_name() {
        assert_eq!(
            require_repo(&json!({ "workspace": "acme", "repo": "app" })).unwrap(),
            ("acme".into(), "app".into())
        );
        assert_eq!(
            require_repo(&json!({ "repo": "acme/app" })).unwrap(),
            ("acme".into(), "app".into())
        );
        assert!(require_repo(&json!({})).is_err());
    }
}
