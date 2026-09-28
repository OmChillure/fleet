//! Control-plane integration API proxy.
//!
//! ```text
//! proxy/
//!   mod.rs, types.rs, sanitize.rs
//!   apps/
//!     github/   client.rs + tools/{reads,writes}
//!     jira/     client.rs + tools/issues
//!     linear/   client.rs + tools/issues
//! ```
//!
//! Decrypt OAuth tokens **in process**, call allowlisted tools, return
//! **sanitized** JSON. Secrets never enter sandboxes or browser responses.
//!
//! **Add a tool:** implement under `apps/<provider>/tools/`, register in that
//! app's `OPERATIONS` + `invoke` match.

mod apps;
mod sanitize;
mod types;

pub use apps::{dispatch, operations_catalog_text, operations_for, supported_providers};
pub use sanitize::{contains_secret_keys, sanitize_value};
pub use types::{ProxyCaller, ProxyRequest, ProxyResult, MAX_PROXY_DATA_BYTES};

use std::time::Duration;

use fleet_db::PgPool;
use serde_json::{json, Value};

use crate::crypto::CredentialsKey;

/// Workspace + provider used to load credentials.
///
/// Tool `args` must never retarget this. Callers can put another workspace's
/// `connection_id` / `workspace_id` / `id` in args; lookup stays on
/// [`ProxyRequest::workspace_id`].
/// A stored login was rejected. Missing-scope and not-found stay as normal errors.
fn auth_failure(result: &ProxyResult) -> bool {
    if result.ok {
        return false;
    }
    if result.error_code.as_deref() == Some("token_expired") || result.provider_status == Some(401)
    {
        return true;
    }
    let summary = result.summary.to_ascii_lowercase();
    summary.contains("invalid token")
        || summary.contains("invalid username")
        || summary.contains("bad credentials")
        || summary.contains("token_revoked")
        || summary.contains("invalid_auth")
        || summary.contains("token expired")
}

fn connection_scope(req: &ProxyRequest) -> (uuid::Uuid, String) {
    (req.workspace_id, req.provider.trim().to_lowercase())
}

fn not_connected(provider: &str, operation: &str) -> ProxyResult {
    ProxyResult::err(
        provider,
        operation,
        "not_connected",
        format!("{provider} is not connected — open Workspace → Integrations to connect it"),
        None,
    )
}

/// Invoke an allowlisted integration operation.
///
/// Tokens are decrypted here, used for the outbound call, and never included
/// in [`ProxyResult`].
pub async fn invoke(pool: &PgPool, key: &CredentialsKey, req: ProxyRequest) -> ProxyResult {
    let (workspace_id, provider) = connection_scope(&req);
    let operation = req.operation.trim().to_lowercase();

    if !supported_providers().contains(&provider.as_str()) {
        return ProxyResult::err(
            &provider,
            &operation,
            "unknown_provider",
            format!(
                "provider `{provider}` is not supported by the proxy; supported: {}",
                supported_providers().join(", ")
            ),
            None,
        );
    }

    if operations_for(&provider).is_empty()
        || !operations_for(&provider).contains(&operation.as_str())
    {
        return ProxyResult::err(
            &provider,
            &operation,
            "unknown_op",
            format!(
                "unknown operation `{operation}` for {provider}; valid: {}",
                operations_for(&provider).join(", ")
            ),
            None,
        );
    }

    let conn =
        match fleet_db::repos::integrations::find_proxy_connection(pool, workspace_id, &provider)
            .await
        {
            Ok(Some(c)) => c,
            Ok(None) => return not_connected(&provider, &operation),
            Err(e) => {
                tracing::error!(
                    workspace_id = %workspace_id,
                    provider = %provider,
                    error = %e,
                    "proxy load connection failed"
                );
                return ProxyResult::err(
                    &provider,
                    &operation,
                    "internal",
                    "failed to load integration connection",
                    None,
                );
            }
        };
    let was_error = conn.status == "error";

    let http = match reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("agent-fleet-integration-proxy/0.1")
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return ProxyResult::err(
                &provider,
                &operation,
                "internal",
                format!("http client: {e}"),
                None,
            );
        }
    };

    let access_token = match resolve_access_token(pool, key, &http, &conn, &operation).await {
        Ok(token) => token,
        Err(err) => {
            // Mint failures return before dispatch. A 401 still has to persist
            // or the card stays Connected with no last_error. 403 stays upstream.
            if err.error_code.as_deref() == Some("token_expired") {
                if let Err(e) = fleet_db::repos::integrations::mark_error(
                    pool,
                    workspace_id,
                    &provider,
                    &err.summary,
                )
                .await
                {
                    tracing::error!(
                        workspace_id = %workspace_id,
                        provider = %provider,
                        error = %e,
                        "failed to persist token_expired on connection"
                    );
                }
            }
            return err;
        }
    };

    tracing::info!(
        workspace_id = %workspace_id,
        provider = %provider,
        operation = %operation,
        caller = req.caller.kind(),
        user_id = ?req.caller.user_id(),
        "integration proxy invoke"
    );

    let mut result = apps::dispatch(
        &provider,
        &http,
        &access_token,
        &operation,
        &req.args,
        &conn.config,
    )
    .await;

    drop(access_token);

    if result.ok {
        result.data = sanitize_value(&result.data);
        if was_error {
            if let Err(e) =
                fleet_db::repos::integrations::clear_error(pool, workspace_id, &provider).await
            {
                tracing::error!(
                    workspace_id = %workspace_id,
                    provider = %provider,
                    error = %e,
                    "failed to clear integration error after a successful call"
                );
            }
        }
        if provider == "github" && operation == "list_repos" {
            let repos: Vec<&str> = result.data["items"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| item.get("full_name").and_then(|v| v.as_str()))
                        .collect()
                })
                .unwrap_or_default();
            if let Err(e) = fleet_db::repos::integrations::set_granted_repos(
                pool,
                workspace_id,
                &provider,
                &json!(repos),
            )
            .await
            {
                tracing::error!(
                    workspace_id = %workspace_id,
                    error = %e,
                    "failed to store granted github repos"
                );
            }
        }
    } else if auth_failure(&result) {
        if let Err(e) = fleet_db::repos::integrations::mark_error(
            pool,
            workspace_id,
            &provider,
            &result.summary,
        )
        .await
        {
            tracing::warn!(
                workspace_id = %workspace_id,
                provider = %provider,
                error = %e,
                "could not mark integration connection as errored"
            );
        }
    }

    if contains_secret_keys(&result.data) {
        tracing::error!(
            provider = %provider,
            operation = %operation,
            "proxy result still contained secret-like keys; redacting"
        );
        result.data = Value::Null;
        result.summary = format!("{} (response redacted for safety)", result.summary);
    }

    tracing::info!(
        workspace_id = %workspace_id,
        provider = %provider,
        operation = %operation,
        ok = result.ok,
        status = ?result.provider_status,
        error_code = ?result.error_code,
        "integration proxy done"
    );

    result
}

async fn resolve_access_token(
    pool: &PgPool,
    key: &CredentialsKey,
    http: &reqwest::Client,
    conn: &fleet_db::IntegrationConnection,
    operation: &str,
) -> Result<String, ProxyResult> {
    let provider = conn.provider.as_str();
    let installation_id = conn
        .config
        .get("installation_id")
        .and_then(|v| v.as_i64())
        .filter(|id| *id > 0);
    if provider == "github" {
        if let Some(installation_id) = installation_id {
            return github_installation_token(pool, key, http, conn, installation_id, operation)
                .await;
        }
    }
    let creds = match key.decrypt(&conn.credentials) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(
                workspace_id = %conn.workspace_id,
                provider,
                error = %e,
                "proxy decrypt failed"
            );
            return Err(ProxyResult::err(
                provider,
                operation,
                "decrypt_failed",
                "failed to decrypt credentials (check INTEGRATIONS_ENCRYPTION_KEY)",
                None,
            ));
        }
    };
    let creds = if provider == "github" {
        refresh_github_user_credentials(pool, key, http, conn, creds).await
    } else {
        creds
    };
    match creds.get("access_token").and_then(|v| v.as_str()) {
        Some(t) if !t.trim().is_empty() => Ok(t.to_string()),
        _ => Err(ProxyResult::err(
            provider,
            operation,
            "token_missing",
            format!("{provider} connection has no access_token — reconnect"),
            None,
        )),
    }
}

async fn refresh_github_user_credentials(
    pool: &PgPool,
    key: &CredentialsKey,
    http: &reqwest::Client,
    conn: &fleet_db::IntegrationConnection,
    creds: Value,
) -> Value {
    let expires_at = creds.get("expires_at").and_then(|v| v.as_i64());
    let refresh = creds
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let (Some(expires_at), Some(refresh)) = (expires_at, refresh) else {
        return creds;
    };
    if chrono::Utc::now().timestamp() + 60 < expires_at {
        return creds;
    }
    let app = match fleet_db::repos::integrations::get_oauth_app(pool, conn.workspace_id, "github")
        .await
    {
        Ok(Some(app)) => app,
        _ => return creds,
    };
    let plain = match key.decrypt(&app.client_secret) {
        Ok(v) => v,
        Err(_) => return creds,
    };
    let Some(client_secret) = plain.get("client_secret").and_then(|v| v.as_str()) else {
        return creds;
    };
    match crate::oauth::refresh_github_user_token(http, &app.client_id, client_secret, refresh)
        .await
    {
        Ok(fresh) => {
            if let Ok(sealed) = key.encrypt(&fresh) {
                let _ = fleet_db::repos::integrations::update_credentials(
                    pool,
                    conn.workspace_id,
                    "github",
                    &sealed,
                )
                .await;
            }
            fresh
        }
        Err(e) => {
            tracing::warn!(
                workspace_id = %conn.workspace_id,
                error = %e,
                "github user token refresh failed"
            );
            creds
        }
    }
}

async fn github_installation_token(
    pool: &PgPool,
    key: &CredentialsKey,
    http: &reqwest::Client,
    conn: &fleet_db::IntegrationConnection,
    installation_id: i64,
    operation: &str,
) -> Result<String, ProxyResult> {
    let provider = conn.provider.as_str();
    let app = match fleet_db::repos::integrations::get_oauth_app(pool, conn.workspace_id, provider)
        .await
    {
        Ok(Some(app)) => app,
        Ok(None) => {
            return Err(ProxyResult::err(
                provider,
                operation,
                "upstream",
                "GitHub App is not stored for this workspace",
                None,
            ));
        }
        Err(e) => {
            tracing::error!(
                workspace_id = %conn.workspace_id,
                error = %e,
                "load github app failed"
            );
            return Err(ProxyResult::err(
                provider,
                operation,
                "internal",
                "failed to load GitHub App",
                None,
            ));
        }
    };
    let plain = match key.decrypt(&app.client_secret) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(
                workspace_id = %conn.workspace_id,
                error = %e,
                "decrypt github app failed"
            );
            return Err(ProxyResult::err(
                provider,
                operation,
                "decrypt_failed",
                "failed to decrypt GitHub App key",
                None,
            ));
        }
    };
    let pem = plain
        .get("pem")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let app_id = plain.get("github_app_id").and_then(|v| v.as_i64());
    let (Some(pem), Some(app_id)) = (pem, app_id) else {
        return Err(ProxyResult::err(
            provider,
            operation,
            "upstream",
            "GitHub App record has no installation key",
            None,
        ));
    };
    match apps::github::mint_installation_token(http, app_id, pem, installation_id).await {
        Ok(token) => Ok(token),
        Err((status, msg)) => {
            let code = if status == 401 {
                "token_expired"
            } else {
                "upstream"
            };
            let status = (status > 0).then_some(status);
            Err(ProxyResult::err(
                provider,
                operation,
                code,
                format!("GitHub API {}: {msg}", status.unwrap_or(0)),
                status,
            ))
        }
    }
}

/// Format a proxy result as markdown for embedding into agent task specs.
pub fn format_context_markdown(results: &[ProxyResult]) -> String {
    if results.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n\n## Integration context (control-plane fetch; no secrets)\n\n");
    for r in results {
        out.push_str(&format!(
            "### {} `{}` — {}\n\n",
            r.provider, r.operation, r.summary
        ));
        if r.ok {
            let pretty = serde_json::to_string_pretty(&r.data).unwrap_or_else(|_| "{}".into());
            out.push_str("```json\n");
            out.push_str(&pretty);
            out.push_str("\n```\n\n");
        } else {
            out.push_str(&format!(
                "_Failed ({}): {}_\n\n",
                r.error_code.as_deref().unwrap_or("error"),
                r.summary
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// HTTP client that can only reach a local sink. Any connect is an
    /// outbound GitHub attempt (api.github.com is HTTPS — CONNECT hits us).
    struct CreatePrSink {
        hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        http: reqwest::Client,
    }

    impl Drop for CreatePrSink {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    impl CreatePrSink {
        fn bind() -> Self {
            use std::io::{Read, Write};
            use std::net::TcpListener;
            use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
            use std::sync::Arc;
            use std::thread;

            let listener = TcpListener::bind("127.0.0.1:0").expect("bind create_pr sink");
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
                            let _ = stream.set_read_timeout(Some(Duration::from_millis(50)));
                            let mut buf = [0u8; 128];
                            let _ = stream.read(&mut buf);
                            let _ = stream.write_all(
                                b"HTTP/1.1 503 sink\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                            );
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => break,
                    }
                }
            });
            let http = reqwest::Client::builder()
                .proxy(reqwest::Proxy::all(format!("http://{addr}")).expect("proxy"))
                .timeout(Duration::from_secs(2))
                .build()
                .expect("client");
            Self { hits, stop, http }
        }

        fn hits(&self) -> usize {
            self.hits.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    /// Unique published catalog names (gitlab/jira/linear/figma/gdrive/teams/
    /// discord/asana/trello plus github `merge_pr` / `update_issue`).
    #[test]
    fn operations_listed() {
        const CATALOG: &[(&str, &[&str])] = &[
            ("github", &["update_issue", "merge_pr"]),
            (
                "gitlab",
                &[
                    "list_mrs",
                    "commit_files",
                    "create_mr",
                    "merge_mr",
                    "update_issue",
                ],
            ),
            (
                "gitea",
                &[
                    "list_repos",
                    "put_file",
                    "list_prs",
                    "create_pr",
                    "merge_pr",
                    "merge_mr",
                    "update_issue",
                    "add_comment",
                ],
            ),
            ("jira", &["get_issue", "create_issue"]),
            (
                "linear",
                &["list_issues", "create_issue", "list_workflow_states"],
            ),
            ("figma", &["get_file"]),
            ("google_drive", &["list_files"]),
            ("teams", &["send_chat_message"]),
            ("discord", &["list_guilds"]),
            ("trello", &["list_boards", "create_card"]),
            ("asana", &["list_tasks", "create_task"]),
        ];
        for (provider, expected) in CATALOG {
            let listed = operations_for(provider);
            for op in *expected {
                assert!(
                    listed.contains(op),
                    "published {provider} allowlist must include `{op}`; got {listed:?}"
                );
            }
        }
    }

    #[test]
    fn auth_failure_is_401_or_rejected_token_not_a_missing_resource() {
        let denied = ProxyResult::err(
            "gitea",
            "add_comment",
            "upstream",
            "invalid username, password or token (401)",
            Some(401),
        );
        assert!(auth_failure(&denied));
        let expired = ProxyResult::err(
            "github",
            "get_issue",
            "token_expired",
            "GitHub API 403: Resource not accessible by integration",
            Some(403),
        );
        assert!(auth_failure(&expired));
        let missing = ProxyResult::err(
            "gitea",
            "get_issue",
            "upstream",
            "not found (404)",
            Some(404),
        );
        assert!(!auth_failure(&missing));
        let ok = ProxyResult::ok_result("gitea", "list_repos", "ok", json!({}), Some(200));
        assert!(!auth_failure(&ok));
    }

    /// Docs §7.3 / README allowlist for every non-github provider.
    #[test]
    fn docs_7_3_non_github_allowlists_complete() {
        const DOC_NON_GITHUB: &[(&str, &[&str])] = &[
            (
                "gitlab",
                &[
                    "list_repos",
                    "list_issues",
                    "get_issue",
                    "list_mrs",
                    "get_mr",
                    "get_file",
                    "list_commits",
                    "commit_files",
                    "create_branch",
                    "create_mr",
                    "create_issue",
                    "add_comment",
                    "merge_mr",
                ],
            ),
            (
                "jira",
                &[
                    "search_issues",
                    "get_issue",
                    "list_projects",
                    "create_issue",
                    "add_comment",
                    "list_transitions",
                    "transition_issue",
                ],
            ),
            (
                "linear",
                &[
                    "list_issues",
                    "get_issue",
                    "list_teams",
                    "create_issue",
                    "update_issue",
                    "add_comment",
                ],
            ),
            (
                "slack",
                &[
                    "list_channels",
                    "list_users",
                    "post_message",
                    "get_channel_history",
                ],
            ),
            (
                "figma",
                &[
                    "get_me",
                    "get_file",
                    "get_file_nodes",
                    "list_comments",
                    "list_team_projects",
                    "get_project_files",
                ],
            ),
            (
                "notion",
                &[
                    "search",
                    "get_page",
                    "get_database",
                    "query_database",
                    "get_block_children",
                    "create_page",
                    "append_blocks",
                ],
            ),
            ("google_drive", &["list_files", "get_file", "export_file"]),
            (
                "teams",
                &[
                    "get_me",
                    "list_chats",
                    "list_teams",
                    "list_channels",
                    "list_chat_messages",
                    "send_chat_message",
                    "send_channel_message",
                ],
            ),
            ("discord", &["get_me", "list_guilds"]),
            (
                "trello",
                &[
                    "list_boards",
                    "list_lists",
                    "list_cards",
                    "get_card",
                    "create_card",
                    "update_card",
                    "add_comment",
                ],
            ),
            (
                "asana",
                &[
                    "list_workspaces",
                    "list_projects",
                    "list_tasks",
                    "get_task",
                    "create_task",
                    "update_task",
                    "add_comment",
                ],
            ),
            (
                "bitbucket",
                &[
                    "list_repos",
                    "list_issues",
                    "get_issue",
                    "list_prs",
                    "get_pr",
                    "get_file",
                    "list_commits",
                    "commit_files",
                    "create_branch",
                    "create_pr",
                    "create_issue",
                    "add_comment",
                ],
            ),
            (
                "confluence",
                &[
                    "search",
                    "list_spaces",
                    "list_pages",
                    "get_page",
                    "create_page",
                    "update_page",
                    "add_comment",
                ],
            ),
        ];
        for (provider, expected) in DOC_NON_GITHUB {
            let listed = operations_for(provider);
            for op in *expected {
                assert!(
                    listed.contains(op),
                    "docs §7.3 / README {provider} allowlist must include `{op}`; got {listed:?}"
                );
            }
        }
    }

    /// Listed names must have a dispatch arm (not only OPERATIONS).
    #[tokio::test]
    async fn every_listed_op_dispatch_is_not_unknown_op() {
        let http = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:1").expect("blackhole proxy"))
            .timeout(Duration::from_millis(200))
            .build()
            .expect("http");
        for provider in supported_providers() {
            let config = match *provider {
                "jira" | "confluence" => json!({ "cloud_id": "dummy" }),
                "trello" => json!({ "api_key": "dummy" }),
                _ => json!({}),
            };
            for op in operations_for(provider) {
                let result = dispatch(provider, &http, "token", op, &json!({}), &config).await;
                assert_ne!(
                    result.error_code.as_deref(),
                    Some("unknown_op"),
                    "listed {provider} `{op}` must dispatch, not unknown_op: {result:?}"
                );
            }
        }
    }

    /// Listed `merge_pr` must be wired (sibling dispatch covers `create_pr`).
    #[tokio::test]
    async fn github_merge_pr_dispatch_is_bad_args_not_unknown_op() {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("http");
        let missing = dispatch("github", &http, "token", "merge_pr", &json!({}), &json!({})).await;
        assert!(
            !missing.ok,
            "merge_pr missing owner/number must be ok:false: {missing:?}"
        );
        assert_eq!(
            missing.error_code.as_deref(),
            Some("bad_args"),
            "merge_pr must be dispatched (bad_args), not unknown_op: {missing:?}"
        );
    }

    /// Listed `create_pr` must be wired (GET repo / POST pulls), not only
    /// present on OPERATIONS. A local sink is not GitHub.
    #[tokio::test]
    async fn github_create_pr_allowlisted_unknown_decrypt_sanitized() {
        const LEAK: &str = "gho_CREATE_PR_PROXY_MUST_NOT_ECHO_aa11";
        let sink = CreatePrSink::bind();
        let forwarded = super::apps::dispatch(
            "github",
            &sink.http,
            LEAK,
            "create_pr",
            &json!({
                "owner": "acme",
                "repo": "app",
                "title": "add README",
                "head": "agent-fleet/changes",
                "access_token": LEAK,
            }),
            &json!({}),
        )
        .await;
        assert!(
            !forwarded.ok,
            "sink is not GitHub — create_pr must be ok:false: {forwarded:?}"
        );
        assert_ne!(
            forwarded.error_code.as_deref(),
            Some("unknown_op"),
            "wired create_pr must not become unknown_op: {forwarded:?}"
        );
        assert_ne!(
            forwarded.error_code.as_deref(),
            Some("bad_args"),
            "valid create_pr args must reach HTTP, not fail locally: {forwarded:?}"
        );
        assert!(
            sink.hits() > 0,
            "allowlisted create_pr must attempt GitHub (sink saw 0 hits): {forwarded:?}"
        );
        let forwarded_s = serde_json::to_string(&forwarded).expect("serialize forwarded");
        assert!(
            !forwarded_s.contains(LEAK),
            "create_pr upstream result must not echo the token: {forwarded_s}"
        );
    }

    /// Unknown github op is fail-closed locally — never a GitHub CONNECT.
    #[tokio::test]
    async fn github_dump_access_token_dispatch_is_unknown_op_without_outbound() {
        const LEAK: &str = "gho_CREATE_PR_PROXY_MUST_NOT_ECHO_aa11";
        let sink = CreatePrSink::bind();
        let unknown = super::apps::dispatch(
            "github",
            &sink.http,
            LEAK,
            "dump_access_token",
            &json!({
                "access_token": LEAK,
                "token": LEAK,
                "owner": "acme",
                "repo": "app",
            }),
            &json!({}),
        )
        .await;
        assert!(
            !unknown.ok,
            "unknown github op must be ok:false: {unknown:?}"
        );
        assert_eq!(
            unknown.error_code.as_deref(),
            Some("unknown_op"),
            "unknown github op must not be forwarded as create_pr: {unknown:?}"
        );
        assert_eq!(
            sink.hits(),
            0,
            "unknown op must not hit GitHub (sink saw {}): {unknown:?}",
            sink.hits()
        );
        let unknown_s = serde_json::to_string(&unknown).expect("serialize unknown");
        assert!(
            !unknown_s.contains(LEAK),
            "unknown_op dispatch must not echo the token: {unknown_s}"
        );
    }

    /// create_pr validates title and head locally before any outbound HTTP.
    #[tokio::test]
    async fn github_create_pr_missing_title_or_head_is_bad_args() {
        let sink = CreatePrSink::bind();
        let no_title = super::apps::dispatch(
            "github",
            &sink.http,
            "token",
            "create_pr",
            &json!({ "owner": "acme", "repo": "app" }),
            &json!({}),
        )
        .await;
        assert_eq!(
            no_title.error_code.as_deref(),
            Some("bad_args"),
            "owner+repo without title must be bad_args: {no_title:?}"
        );
        let no_head = super::apps::dispatch(
            "github",
            &sink.http,
            "token",
            "create_pr",
            &json!({
                "owner": "acme",
                "repo": "app",
                "title": "add README",
            }),
            &json!({}),
        )
        .await;
        assert_eq!(
            no_head.error_code.as_deref(),
            Some("bad_args"),
            "owner+repo+title without head/branch must be bad_args: {no_head:?}"
        );
        assert_eq!(
            sink.hits(),
            0,
            "missing title/head must not hit GitHub (sink saw {})",
            sink.hits()
        );
    }
}
