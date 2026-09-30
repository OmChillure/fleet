//! Register new Gogs tools in OPERATIONS + match below.

mod reads;
mod writes;

use serde_json::Value;

use crate::proxy::types::ProxyResult;

pub const OPERATIONS: &[&str] = &[
    "list_repos",
    "list_issues",
    "get_issue",
    "list_mrs",
    "get_mr",
    "get_file",
    "list_commits",
    "put_file",
    "commit_files",
    "create_branch",
    "create_mr",
    "list_prs",
    "get_pr",
    "create_pr",
    "merge_pr",
    "create_issue",
    "update_issue",
    "add_comment",
    "merge_mr",
];

pub async fn invoke(
    http: &reqwest::Client,
    token: &str,
    config: &Value,
    operation: &str,
    args: &Value,
) -> ProxyResult {
    let base = match super::client::base_url(config) {
        Ok(b) => b,
        Err(e) => {
            return ProxyResult::err("gogs", operation, "bad_config", e, None);
        }
    };
    match operation {
        "list_repos" => reads::list_repos(http, token, &base, args).await,
        "list_issues" => reads::list_issues(http, token, &base, args).await,
        "get_issue" => reads::get_issue(http, token, &base, args).await,
        "list_mrs" | "list_prs" => reads::list_mrs(http, token, &base, args).await,
        "get_mr" | "get_pr" => reads::get_mr(http, token, &base, args).await,
        "get_file" => reads::get_file(http, token, &base, args).await,
        "list_commits" => reads::list_commits(http, token, &base, args).await,
        "put_file" => writes::put_file(http, token, &base, args).await,
        "commit_files" => writes::commit_files(http, token, &base, args).await,
        "create_branch" => writes::create_branch(http, token, &base, args).await,
        "create_mr" | "create_pr" => writes::create_mr(http, token, &base, args).await,
        "create_issue" => writes::create_issue(http, token, &base, args).await,
        "update_issue" => writes::update_issue(http, token, &base, args).await,
        "add_comment" => writes::add_comment(http, token, &base, args).await,
        "merge_mr" | "merge_pr" => writes::merge_mr(http, token, &base, args).await,
        other => ProxyResult::err(
            "gogs",
            other,
            "unknown_op",
            format!(
                "unknown gogs operation `{other}`; valid: {}",
                OPERATIONS.join(", ")
            ),
            None,
        ),
    }
}

#[cfg(test)]
mod docker_live {
    use super::invoke;
    use serde_json::json;

    fn live_env() -> Option<(String, String)> {
        let token = std::env::var("GOGS_TOKEN").ok().filter(|s| !s.is_empty())?;
        let base = std::env::var("GOGS_BASE_URL").unwrap_or_else(|_| "http://localhost:3034".into());
        Some((token, base))
    }

    /// Hits the Compose Gogs container (scripts/gogs-up.sh). Ignored unless
    /// GOGS_TOKEN is set and `--ignored` is passed.
    #[tokio::test]
    #[ignore = "needs Docker Gogs: GOGS_BASE_URL and GOGS_TOKEN from .tmp/gogs.env"]
    async fn docker_gogs_repos_issues_and_files() {
        let (token, base) = live_env().expect("set GOGS_TOKEN (and optional GOGS_BASE_URL)");
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap();
        let config = json!({ "base_url": base });
        let repo = json!({ "owner": "worms", "repo": "game" });

        let repos = invoke(&http, &token, &config, "list_repos", &json!({})).await;
        assert!(repos.ok, "list_repos: {}", repos.summary);
        let names: Vec<_> = repos.data["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["full_name"].as_str())
            .collect();
        assert!(
            names.iter().any(|n| *n == "worms/game"),
            "worms/game missing: {names:?}"
        );

        let issues = invoke(&http, &token, &config, "list_issues", &repo).await;
        assert!(issues.ok, "list_issues: {}", issues.summary);
        assert!(issues.data["count"].as_u64().unwrap_or(0) >= 1, "{}", issues.summary);

        let one = invoke(
            &http,
            &token,
            &config,
            "get_issue",
            &json!({ "owner": "worms", "repo": "game", "number": 1 }),
        )
        .await;
        assert!(one.ok, "get_issue: {}", one.summary);
        assert_eq!(one.data["number"], 1);

        let comment = invoke(
            &http,
            &token,
            &config,
            "add_comment",
            &json!({
                "owner": "worms",
                "repo": "game",
                "number": 1,
                "body": "docker live check"
            }),
        )
        .await;
        assert!(comment.ok, "add_comment: {}", comment.summary);

        let commits = invoke(
            &http,
            &token,
            &config,
            "list_commits",
            &json!({ "owner": "worms", "repo": "game", "sha": "master" }),
        )
        .await;
        assert!(commits.ok, "list_commits: {}", commits.summary);
        assert!(commits.data["count"].as_u64().unwrap_or(0) >= 1, "{}", commits.summary);

        let readme = invoke(
            &http,
            &token,
            &config,
            "get_file",
            &json!({ "owner": "worms", "repo": "game", "path": "README.md", "ref": "master" }),
        )
        .await;
        assert!(readme.ok, "get_file: {}", readme.summary);
        assert!(
            readme.data["content"].as_str().unwrap_or("").contains("game"),
            "{}",
            readme.summary
        );

        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let path = format!("docker-live-{stamp}.txt");
        let wrote = invoke(
            &http,
            &token,
            &config,
            "put_file",
            &json!({
                "owner": "worms",
                "repo": "game",
                "path": path,
                "content": "gogs docker live",
                "message": "test: docker live file",
                "branch": "master"
            }),
        )
        .await;
        assert!(wrote.ok, "put_file: {}", wrote.summary);

        let again = invoke(
            &http,
            &token,
            &config,
            "get_file",
            &json!({ "owner": "worms", "repo": "game", "path": path, "ref": "master" }),
        )
        .await;
        assert!(again.ok, "get_file written: {}", again.summary);
        assert_eq!(again.data["content"].as_str().unwrap_or("").trim(), "gogs docker live");

        let created = invoke(
            &http,
            &token,
            &config,
            "create_issue",
            &json!({
                "owner": "worms",
                "repo": "game",
                "title": format!("docker live {stamp}"),
                "body": "created by the docker gogs test"
            }),
        )
        .await;
        assert!(created.ok, "create_issue: {}", created.summary);
        let number = created.data["number"].as_u64().expect("issue number");
        let closed = invoke(
            &http,
            &token,
            &config,
            "update_issue",
            &json!({
                "owner": "worms",
                "repo": "game",
                "number": number,
                "state": "closed"
            }),
        )
        .await;
        assert!(closed.ok, "update_issue: {}", closed.summary);
        assert_eq!(closed.data["state"], "closed");
    }
}
