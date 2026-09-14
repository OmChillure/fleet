//! GitHub write tools.

use serde_json::{json, Value};

use super::super::client::*;
use crate::proxy::types::ProxyResult;

pub(crate) async fn put_file(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (owner, repo) = match require_owner_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("github", "put_file", "bad_args", e, None),
    };
    let path = match args.get("path").and_then(|v| v.as_str()).map(str::trim) {
        Some(p) if !p.is_empty() && !p.starts_with('/') => p.to_string(),
        _ => {
            return ProxyResult::err(
                "github",
                "put_file",
                "bad_args",
                "path is required (e.g. README.md, no leading /)",
                None,
            )
        }
    };
    let content = match args.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return ProxyResult::err(
                "github",
                "put_file",
                "bad_args",
                "content is required (UTF-8 text)",
                None,
            )
        }
    };
    if content.len() > MAX_FILE_BYTES {
        return ProxyResult::err(
            "github",
            "put_file",
            "bad_args",
            format!("content too large (max {MAX_FILE_BYTES} bytes)"),
            None,
        );
    }
    let message = args
        .get("message")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("chore: update file via Agent Fleet")
        .to_string();
    let branch = args
        .get("branch")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());

    // If file exists, GitHub requires the current blob sha.
    let mut get_url = format!(
        "https://api.github.com/repos/{owner}/{repo}/contents/{}",
        path_enc(&path)
    );
    if let Some(ref b) = branch {
        get_url.push_str(&format!("?ref={}", urlencoding(b)));
    }
    let (get_status, existing) = github_get(http, token, &get_url).await;
    let sha = if (200..300).contains(&get_status) {
        existing
            .get("sha")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    } else {
        None
    };

    let mut body = json!({
        "message": message,
        "content": b64(content.as_bytes()),
    });
    if let Some(b) = branch {
        body["branch"] = json!(b);
    }
    let is_update = sha.is_some();
    if let Some(ref s) = sha {
        body["sha"] = json!(s);
    }

    let url = format!(
        "https://api.github.com/repos/{owner}/{repo}/contents/{}",
        path_enc(&path)
    );
    let (status, res) = github_put(http, token, &url, &body).await;
    if !(200..300).contains(&status) {
        return upstream_err("put_file", status, &res);
    }
    let commit_sha = res
        .pointer("/commit/sha")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let html = res
        .pointer("/content/html_url")
        .or_else(|| res.pointer("/commit/html_url"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    ProxyResult::ok_result(
        "github",
        "put_file",
        format!("Wrote `{path}` on {owner}/{repo}"),
        json!({
            "owner": owner,
            "repo": repo,
            "path": path,
            "commit_sha": commit_sha,
            "html_url": html,
            "updated": is_update,
        }),
        Some(status),
    )
}

/// Commit many files in one commit (Git Data API). Creates branch if missing.
///
/// Args: owner, repo, branch, message, files: [{path, content}], optional base_branch
pub(crate) async fn commit_files(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (owner, repo) = match require_owner_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("github", "commit_files", "bad_args", e, None),
    };
    let branch = args
        .get("branch")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("agent-fleet/changes")
        .to_string();
    let message = args
        .get("message")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("chore: apply files via Agent Fleet")
        .to_string();
    let files = match args.get("files").and_then(|v| v.as_array()) {
        Some(a) if !a.is_empty() => a,
        _ => {
            return ProxyResult::err(
                "github",
                "commit_files",
                "bad_args",
                "files must be a non-empty array of {path, content}",
                None,
            )
        }
    };
    if files.len() > MAX_COMMIT_FILES {
        return ProxyResult::err(
            "github",
            "commit_files",
            "bad_args",
            format!("too many files (max {MAX_COMMIT_FILES})"),
            None,
        );
    }

    let (st, repo_body) = github_get(
        http,
        token,
        &format!("https://api.github.com/repos/{owner}/{repo}"),
    )
    .await;
    if !(200..300).contains(&st) {
        return upstream_err("commit_files", st, &repo_body);
    }
    let default_branch = repo_body
        .get("default_branch")
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    if branch == default_branch || branch == "main" || branch == "master" {
        return ProxyResult::err(
            "github",
            "commit_files",
            "forbidden",
            "refusing to commit to the default branch; pass a feature branch",
            None,
        );
    }
    let base_branch = args
        .get("base_branch")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_branch);

    let (st, base_ref) = github_get(
        http,
        token,
        &format!("https://api.github.com/repos/{owner}/{repo}/git/ref/heads/{base_branch}"),
    )
    .await;
    if !(200..300).contains(&st) {
        return upstream_err("commit_files", st, &base_ref);
    }
    let base_commit_sha = base_ref
        .pointer("/object/sha")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    if base_commit_sha.is_empty() {
        return ProxyResult::err(
            "github",
            "commit_files",
            "upstream",
            "could not resolve base branch commit",
            None,
        );
    }

    let (st, base_commit) = github_get(
        http,
        token,
        &format!("https://api.github.com/repos/{owner}/{repo}/git/commits/{base_commit_sha}"),
    )
    .await;
    if !(200..300).contains(&st) {
        return upstream_err("commit_files", st, &base_commit);
    }
    let base_tree = base_commit
        .pointer("/tree/sha")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    // Ensure target branch exists (create from base if needed).
    let (st_br, br_ref) = github_get(
        http,
        token,
        &format!("https://api.github.com/repos/{owner}/{repo}/git/ref/heads/{branch}"),
    )
    .await;
    let mut parent_sha = base_commit_sha.clone();
    let mut parent_tree = base_tree.clone();
    if (200..300).contains(&st_br) {
        parent_sha = br_ref
            .pointer("/object/sha")
            .and_then(|v| v.as_str())
            .unwrap_or(&base_commit_sha)
            .to_string();
        let (st, c) = github_get(
            http,
            token,
            &format!("https://api.github.com/repos/{owner}/{repo}/git/commits/{parent_sha}"),
        )
        .await;
        if (200..300).contains(&st) {
            parent_tree = c
                .pointer("/tree/sha")
                .and_then(|v| v.as_str())
                .unwrap_or(&base_tree)
                .to_string();
        }
    } else {
        let (st, created) = github_post(
            http,
            token,
            &format!("https://api.github.com/repos/{owner}/{repo}/git/refs"),
            &json!({
                "ref": format!("refs/heads/{branch}"),
                "sha": base_commit_sha,
            }),
        )
        .await;
        if !(200..300).contains(&st) {
            return upstream_err("commit_files", st, &created);
        }
    }

    let mut tree_items = Vec::new();
    let mut paths = Vec::new();
    for f in files {
        let path = match f.get("path").and_then(|v| v.as_str()).map(str::trim) {
            Some(p) if !p.is_empty() && !p.starts_with('/') => p,
            _ => {
                return ProxyResult::err(
                    "github",
                    "commit_files",
                    "bad_args",
                    "each file needs a relative path",
                    None,
                )
            }
        };
        let content = match f.get("content").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => {
                return ProxyResult::err(
                    "github",
                    "commit_files",
                    "bad_args",
                    format!("file `{path}` missing content"),
                    None,
                )
            }
        };
        if content.len() > MAX_FILE_BYTES {
            return ProxyResult::err(
                "github",
                "commit_files",
                "bad_args",
                format!("file `{path}` too large (max {MAX_FILE_BYTES} bytes)"),
                None,
            );
        }
        let (st, blob) = github_post(
            http,
            token,
            &format!("https://api.github.com/repos/{owner}/{repo}/git/blobs"),
            &json!({
                "content": b64(content.as_bytes()),
                "encoding": "base64",
            }),
        )
        .await;
        if !(200..300).contains(&st) {
            return upstream_err("commit_files", st, &blob);
        }
        let blob_sha = blob.get("sha").and_then(|v| v.as_str()).unwrap_or("");
        if blob_sha.is_empty() {
            return ProxyResult::err(
                "github",
                "commit_files",
                "upstream",
                format!("blob create failed for `{path}`"),
                None,
            );
        }
        tree_items.push(json!({
            "path": path,
            "mode": "100644",
            "type": "blob",
            "sha": blob_sha,
        }));
        paths.push(path.to_string());
    }

    let (st, tree) = github_post(
        http,
        token,
        &format!("https://api.github.com/repos/{owner}/{repo}/git/trees"),
        &json!({
            "base_tree": parent_tree,
            "tree": tree_items,
        }),
    )
    .await;
    if !(200..300).contains(&st) {
        return upstream_err("commit_files", st, &tree);
    }
    let tree_sha = tree.get("sha").and_then(|v| v.as_str()).unwrap_or("");

    let (st, commit) = github_post(
        http,
        token,
        &format!("https://api.github.com/repos/{owner}/{repo}/git/commits"),
        &json!({
            "message": message,
            "tree": tree_sha,
            "parents": [parent_sha],
        }),
    )
    .await;
    if !(200..300).contains(&st) {
        return upstream_err("commit_files", st, &commit);
    }
    let commit_sha = commit.get("sha").and_then(|v| v.as_str()).unwrap_or("");
    let commit_url = commit
        .get("html_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let (st, updated) = github_patch(
        http,
        token,
        &format!("https://api.github.com/repos/{owner}/{repo}/git/refs/heads/{branch}"),
        &json!({ "sha": commit_sha, "force": false }),
    )
    .await;
    if !(200..300).contains(&st) {
        return upstream_err("commit_files", st, &updated);
    }

    ProxyResult::ok_result(
        "github",
        "commit_files",
        format!(
            "Committed {} file(s) to {owner}/{repo}@{branch}",
            paths.len()
        ),
        json!({
            "owner": owner,
            "repo": repo,
            "branch": branch,
            "base_branch": base_branch,
            "commit_sha": commit_sha,
            "html_url": commit_url,
            "files": paths,
            "hint": "Use create_pr to open a pull request from this branch.",
        }),
        Some(st),
    )
}

pub(crate) async fn create_branch(
    http: &reqwest::Client,
    token: &str,
    args: &Value,
) -> ProxyResult {
    let (owner, repo) = match require_owner_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("github", "create_branch", "bad_args", e, None),
    };
    let branch = match args.get("branch").and_then(|v| v.as_str()).map(str::trim) {
        Some(b) if !b.is_empty() => b.to_string(),
        _ => {
            return ProxyResult::err(
                "github",
                "create_branch",
                "bad_args",
                "branch is required",
                None,
            )
        }
    };
    let (st, repo_body) = github_get(
        http,
        token,
        &format!("https://api.github.com/repos/{owner}/{repo}"),
    )
    .await;
    if !(200..300).contains(&st) {
        return upstream_err("create_branch", st, &repo_body);
    }
    let default_branch = repo_body
        .get("default_branch")
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    let from = args
        .get("from")
        .or_else(|| args.get("base_branch"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_branch);

    let (st, base_ref) = github_get(
        http,
        token,
        &format!("https://api.github.com/repos/{owner}/{repo}/git/ref/heads/{from}"),
    )
    .await;
    if !(200..300).contains(&st) {
        return upstream_err("create_branch", st, &base_ref);
    }
    let sha = base_ref
        .pointer("/object/sha")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let (st, created) = github_post(
        http,
        token,
        &format!("https://api.github.com/repos/{owner}/{repo}/git/refs"),
        &json!({
            "ref": format!("refs/heads/{branch}"),
            "sha": sha,
        }),
    )
    .await;
    if !(200..300).contains(&st) {
        // 422 often means branch exists — treat as ok-ish
        if st == 422 {
            return ProxyResult::ok_result(
                "github",
                "create_branch",
                format!("Branch `{branch}` already exists on {owner}/{repo}"),
                json!({ "owner": owner, "repo": repo, "branch": branch, "already_exists": true }),
                Some(st),
            );
        }
        return upstream_err("create_branch", st, &created);
    }
    ProxyResult::ok_result(
        "github",
        "create_branch",
        format!("Created branch `{branch}` on {owner}/{repo} from `{from}`"),
        json!({
            "owner": owner,
            "repo": repo,
            "branch": branch,
            "from": from,
            "sha": sha,
        }),
        Some(st),
    )
}

pub(crate) async fn create_pr(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (owner, repo) = match require_owner_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("github", "create_pr", "bad_args", e, None),
    };
    let title = match args.get("title").and_then(|v| v.as_str()).map(str::trim) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => return ProxyResult::err("github", "create_pr", "bad_args", "title is required", None),
    };
    let head = match args
        .get("head")
        .or_else(|| args.get("branch"))
        .and_then(|v| v.as_str())
        .map(str::trim)
    {
        Some(h) if !h.is_empty() => h.to_string(),
        _ => {
            return ProxyResult::err(
                "github",
                "create_pr",
                "bad_args",
                "head (branch name) is required",
                None,
            )
        }
    };
    let origin = github_api_origin();
    let (st, repo_body) = github_get(http, token, &format!("{origin}/repos/{owner}/{repo}")).await;
    if !(200..300).contains(&st) {
        return upstream_err("create_pr", st, &repo_body);
    }
    let default_branch = repo_body
        .get("default_branch")
        .and_then(|v| v.as_str())
        .unwrap_or("main");
    let base = args
        .get("base")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(default_branch)
        .to_string();
    let body_text = args
        .get("body")
        .and_then(|v| v.as_str())
        .unwrap_or("Opened by Agent Fleet (control-plane integration; no tokens in sandboxes).")
        .to_string();
    let draft = args.get("draft").and_then(|v| v.as_bool()).unwrap_or(false);

    let (st, pr) = github_post(
        http,
        token,
        &format!("{origin}/repos/{owner}/{repo}/pulls"),
        &json!({
            "title": title,
            "head": head,
            "base": base,
            "body": body_text,
            "draft": draft,
        }),
    )
    .await;
    if !(200..300).contains(&st) {
        return upstream_err("create_pr", st, &pr);
    }
    ProxyResult::ok_result(
        "github",
        "create_pr",
        format!(
            "Opened PR #{}: {}",
            pr.get("number").and_then(|v| v.as_u64()).unwrap_or(0),
            title
        ),
        map_pr(&pr),
        Some(st),
    )
}

pub(crate) async fn create_issue(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (owner, repo) = match require_owner_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("github", "create_issue", "bad_args", e, None),
    };
    let title = match args.get("title").and_then(|v| v.as_str()).map(str::trim) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => {
            return ProxyResult::err(
                "github",
                "create_issue",
                "bad_args",
                "title is required",
                None,
            );
        }
    };
    let mut payload = json!({ "title": title });
    if let Some(b) = args.get("body").and_then(|v| v.as_str()) {
        payload["body"] = json!(b);
    }
    if let Some(labels) = args.get("labels") {
        payload["labels"] = labels.clone();
    }
    let (status, body) = github_post(
        http,
        token,
        &format!("https://api.github.com/repos/{owner}/{repo}/issues"),
        &payload,
    )
    .await;
    if !(200..300).contains(&status) {
        return upstream_err("create_issue", status, &body);
    }
    ProxyResult::ok_result(
        "github",
        "create_issue",
        format!(
            "Created issue #{} on {owner}/{repo}",
            body.get("number").and_then(|v| v.as_u64()).unwrap_or(0)
        ),
        map_issue(&body),
        Some(status),
    )
}

pub(crate) async fn update_issue(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (owner, repo) = match require_owner_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("github", "update_issue", "bad_args", e, None),
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
                "update_issue",
                "bad_args",
                "number is required",
                None,
            );
        }
    };
    let mut payload = json!({});
    if let Some(state) = args
        .get("state")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let state = state.to_ascii_lowercase();
        if state != "open" && state != "closed" {
            return ProxyResult::err(
                "github",
                "update_issue",
                "bad_args",
                "state must be open or closed",
                None,
            );
        }
        payload["state"] = json!(state);
    }
    if let Some(title) = args.get("title").and_then(|v| v.as_str()) {
        payload["title"] = json!(title);
    }
    if let Some(body) = args
        .get("body")
        .or_else(|| args.get("description"))
        .and_then(|v| v.as_str())
    {
        payload["body"] = json!(body);
    }
    if payload.as_object().map(|m| m.is_empty()).unwrap_or(true) {
        return ProxyResult::err(
            "github",
            "update_issue",
            "bad_args",
            "provide state, title, and/or body",
            None,
        );
    }
    let (status, body) = github_patch(
        http,
        token,
        &format!("https://api.github.com/repos/{owner}/{repo}/issues/{number}"),
        &payload,
    )
    .await;
    if !(200..300).contains(&status) {
        return upstream_err("update_issue", status, &body);
    }
    ProxyResult::ok_result(
        "github",
        "update_issue",
        format!("Updated {owner}/{repo}#{number}"),
        map_issue(&body),
        Some(status),
    )
}

pub(crate) async fn add_comment(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (owner, repo) = match require_owner_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("github", "add_comment", "bad_args", e, None),
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
                "add_comment",
                "bad_args",
                "number is required",
                None,
            );
        }
    };
    let body_text = match args
        .get("body")
        .or_else(|| args.get("comment"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(t) => t.to_string(),
        None => {
            return ProxyResult::err(
                "github",
                "add_comment",
                "bad_args",
                "body is required",
                None,
            );
        }
    };
    let (status, body) = github_post(
        http,
        token,
        &format!("https://api.github.com/repos/{owner}/{repo}/issues/{number}/comments"),
        &json!({ "body": body_text }),
    )
    .await;
    if !(200..300).contains(&status) {
        return upstream_err("add_comment", status, &body);
    }
    ProxyResult::ok_result(
        "github",
        "add_comment",
        format!("Commented on {owner}/{repo}#{number}"),
        json!({
            "id": body.get("id"),
            "html_url": body.get("html_url"),
            "user": body.get("user").and_then(|u| u.get("login")),
        }),
        Some(status),
    )
}

pub(crate) async fn merge_pr(http: &reqwest::Client, token: &str, args: &Value) -> ProxyResult {
    let (owner, repo) = match require_owner_repo(args) {
        Ok(v) => v,
        Err(e) => return ProxyResult::err("github", "merge_pr", "bad_args", e, None),
    };
    let number = match args
        .get("number")
        .or_else(|| args.get("pr"))
        .or_else(|| args.get("pull_number"))
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        }) {
        Some(n) => n,
        None => {
            return ProxyResult::err(
                "github",
                "merge_pr",
                "bad_args",
                "number (PR) is required",
                None,
            );
        }
    };
    let method = args
        .get("merge_method")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("squash");
    if !matches!(method, "merge" | "squash" | "rebase") {
        return ProxyResult::err(
            "github",
            "merge_pr",
            "bad_args",
            "merge_method must be merge, squash, or rebase",
            None,
        );
    }
    let mut payload = json!({ "merge_method": method });
    if let Some(title) = args
        .get("commit_title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        payload["commit_title"] = json!(title);
    }
    let (status, body) = github_put(
        http,
        token,
        &format!("https://api.github.com/repos/{owner}/{repo}/pulls/{number}/merge"),
        &payload,
    )
    .await;
    if !(200..300).contains(&status) {
        return upstream_err("merge_pr", status, &body);
    }
    ProxyResult::ok_result(
        "github",
        "merge_pr",
        format!("Merged PR #{number} on {owner}/{repo} ({method})"),
        json!({
            "merged": body.get("merged"),
            "sha": body.get("sha"),
            "message": body.get("message"),
            "html_url": format!("https://github.com/{owner}/{repo}/pull/{number}"),
        }),
        Some(status),
    )
}

#[cfg(test)]
mod create_pr_tests {
    use super::*;
    use crate::proxy::dispatch;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    const LEAK: &str = "gho_CREATE_PR_OK_MUST_NOT_ECHO_7eaf";

    struct OriginGuard;
    impl Drop for OriginGuard {
        fn drop(&mut self) {
            TEST_GITHUB_API_BASE.with(|c| *c.borrow_mut() = None);
        }
    }

    struct OkPrSink {
        hits: Arc<AtomicUsize>,
        stop: Arc<AtomicBool>,
        origin: String,
    }

    impl Drop for OkPrSink {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
        }
    }

    impl OkPrSink {
        fn bind() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind create_pr ok sink");
            listener.set_nonblocking(true).expect("nonblocking sink");
            let addr = listener.local_addr().expect("sink addr");
            let hits = Arc::new(AtomicUsize::new(0));
            let stop = Arc::new(AtomicBool::new(false));
            let hits_t = hits.clone();
            let stop_t = stop.clone();
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
                                        if let Some(end) = headers_end(&buf) {
                                            let len = content_length(&buf[..end]).unwrap_or(0);
                                            if buf.len() >= end + len {
                                                break;
                                            }
                                        }
                                    }
                                    Err(_) => break,
                                }
                            }
                            let req = String::from_utf8_lossy(&buf);
                            let (status, body) = if req.starts_with("POST ") {
                                (
                                    201,
                                    json!({
                                        "number": 7,
                                        "title": "add README",
                                        "html_url": "https://github.com/acme/app/pull/7",
                                        "access_token": LEAK,
                                    })
                                    .to_string(),
                                )
                            } else {
                                (200, json!({ "default_branch": "main" }).to_string())
                            };
                            let resp = format!(
                                "HTTP/1.1 {status} ok\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                                body.len()
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
            }
        }
    }

    fn headers_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
    }

    fn content_length(headers: &[u8]) -> Option<usize> {
        let text = std::str::from_utf8(headers).ok()?;
        for line in text.split("\r\n") {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            if name.eq_ignore_ascii_case("content-length") {
                return value.trim().parse().ok();
            }
        }
        None
    }

    /// Success path: GET repo 200 then POST /pulls 201; map_pr keeps number and drops access_token.
    #[tokio::test(flavor = "current_thread")]
    async fn github_create_pr_ok_maps_number_and_drops_provider_access_token() {
        let sink = OkPrSink::bind();
        let _guard = OriginGuard;
        TEST_GITHUB_API_BASE.with(|c| *c.borrow_mut() = Some(sink.origin.clone()));
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("http");
        let result = dispatch(
            "github",
            &http,
            "token",
            "create_pr",
            &json!({
                "owner": "acme",
                "repo": "app",
                "title": "add README",
                "head": "agent-fleet/changes",
            }),
            &json!({}),
        )
        .await;
        assert!(result.ok, "create_pr 201 must be ok: {result:?}");
        assert_eq!(
            result.data.get("number"),
            Some(&json!(7)),
            "map_pr must copy number: {result:?}"
        );
        let dump = serde_json::to_string(&result).expect("serialize");
        assert!(
            !dump.contains(LEAK),
            "create_pr success must not echo provider access_token: {dump}"
        );
        assert!(
            sink.hits.load(Ordering::SeqCst) >= 2,
            "create_pr must GET repo then POST /pulls (hits={})",
            sink.hits.load(Ordering::SeqCst)
        );
    }
}
