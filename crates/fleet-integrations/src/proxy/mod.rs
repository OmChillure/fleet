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
pub use types::{MAX_PROXY_DATA_BYTES, ProxyCaller, ProxyRequest, ProxyResult};

use std::time::Duration;

use fleet_db::PgPool;
use serde_json::Value;

use crate::credentials_for;
use crate::crypto::CredentialsKey;

/// Workspace + provider used to load credentials.
///
/// Tool `args` must never retarget this. Callers can put another workspace's
/// `connection_id` / `workspace_id` / `id` in args; lookup stays on
/// [`ProxyRequest::workspace_id`].
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
        match fleet_db::repos::integrations::find_connected(pool, workspace_id, &provider).await {
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

    let creds = match credentials_for(pool, workspace_id, &provider, key).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            return ProxyResult::err(
                &provider,
                &operation,
                "not_connected",
                format!("{provider} credentials missing — reconnect the integration"),
                None,
            );
        }
        Err(e) => {
            tracing::error!(
                workspace_id = %workspace_id,
                provider = %provider,
                error = %e,
                "proxy decrypt failed"
            );
            return ProxyResult::err(
                &provider,
                &operation,
                "decrypt_failed",
                "failed to decrypt credentials (check INTEGRATIONS_ENCRYPTION_KEY)",
                None,
            );
        }
    };

    let access_token = match creds.get("access_token").and_then(|v| v.as_str()) {
        Some(t) if !t.trim().is_empty() => t.to_string(),
        _ => {
            return ProxyResult::err(
                &provider,
                &operation,
                "token_missing",
                format!("{provider} connection has no access_token — reconnect"),
                None,
            );
        }
    };

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
    drop(creds);

    if result.ok {
        result.data = sanitize_value(&result.data);
    } else if result.error_code.as_deref() == Some("token_expired") {
        if let Err(e) = fleet_db::repos::integrations::mark_error(
            pool,
            workspace_id,
            &provider,
            &result.summary,
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

    fn production_src() -> &'static str {
        let src = include_str!("mod.rs");
        match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        }
    }

    fn fn_src<'a>(src: &'a str, sig: &str) -> &'a str {
        let start = src.find(sig).unwrap_or_else(|| panic!("missing {sig}"));
        let after = start + sig.len();
        let end = src[after..]
            .find("\npub async fn ")
            .or_else(|| src[after..].find("\nasync fn "))
            .or_else(|| src[after..].find("\npub fn "))
            .or_else(|| src[after..].find("\nfn "))
            .map(|i| after + i)
            .unwrap_or(src.len());
        &src[start..end]
    }

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
            use std::sync::Arc;
            use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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

    #[test]
    fn operations_listed() {
        assert!(operations_for("github").contains(&"list_prs"));
        assert!(operations_for("github").contains(&"commit_files"));
        assert!(operations_for("github").contains(&"get_file"));
        assert!(operations_for("github").contains(&"update_issue"));
        assert!(operations_for("github").contains(&"merge_pr"));
        assert!(operations_for("gitlab").contains(&"list_mrs"));
        assert!(operations_for("gitlab").contains(&"commit_files"));
        assert!(operations_for("gitlab").contains(&"create_mr"));
        assert!(operations_for("gitlab").contains(&"merge_mr"));
        assert!(operations_for("gitlab").contains(&"update_issue"));
        assert!(operations_for("jira").contains(&"get_issue"));
        assert!(operations_for("jira").contains(&"create_issue"));
        assert!(operations_for("linear").contains(&"list_issues"));
        assert!(operations_for("linear").contains(&"create_issue"));
        assert!(operations_for("linear").contains(&"list_workflow_states"));
        assert!(operations_for("slack").contains(&"post_message"));
        assert!(operations_for("figma").contains(&"get_file"));
        assert!(operations_for("notion").contains(&"search"));
        assert!(operations_for("google_drive").contains(&"list_files"));
        assert!(operations_for("teams").contains(&"send_chat_message"));
        assert!(operations_for("discord").contains(&"list_guilds"));
        assert!(operations_for("trello").contains(&"list_boards"));
        assert!(operations_for("trello").contains(&"create_card"));
        assert!(operations_for("asana").contains(&"list_tasks"));
        assert!(operations_for("asana").contains(&"create_task"));
        assert!(operations_for("bitbucket").contains(&"create_pr"));
        assert!(operations_for("confluence").contains(&"create_page"));
        assert_eq!(supported_providers().len(), 14);
    }

    #[test]
    fn context_markdown_no_secrets() {
        let r = ProxyResult::ok_result(
            "github",
            "list_prs",
            "ok",
            json!({ "items": [{ "title": "x", "access_token": "nope" }] }),
            Some(200),
        );
        let mut r = r;
        r.data = sanitize_value(&r.data);
        assert!(!contains_secret_keys(&r.data));
        let md = format_context_markdown(&[r]);
        assert!(!md.contains("nope"));
        assert!(md.contains("Integration context"));
    }

    /// Docs §7.2 `github_create_pr` / §7.3 `create_pr`. Allowlisted.
    /// Unknown op and decrypt failure are `ok: false`. Result sanitized.
    /// Workspace lookup ignores args.
    #[tokio::test]
    async fn github_create_pr_allowlisted_unknown_decrypt_sanitized() {
        // Docs §7.3 github row — literals, not OPERATIONS.
        const DOC_GITHUB_OPS: &[&str] = &[
            "list_repos",
            "list_issues",
            "get_issue",
            "list_prs",
            "get_pr",
            "get_file",
            "list_commits",
            "put_file",
            "commit_files",
            "create_branch",
            "create_pr",
            "create_issue",
            "add_comment",
        ];
        let ops = operations_for("github");
        for op in DOC_GITHUB_OPS {
            assert!(
                ops.contains(op),
                "docs §7.3 github allowlist must include `{op}`"
            );
        }
        assert!(
            ops.contains(&"create_pr"),
            "github_create_pr is allowlisted create_pr"
        );
        assert!(
            !ops.contains(&"dump_access_token") && !ops.contains(&"delete_repo"),
            "secret-dump / destructive ops must not sneak onto the github allowlist"
        );

        const LEAK: &str = "gho_CREATE_PR_PROXY_MUST_NOT_ECHO_aa11";
        let dirty = json!({
            "number": 7,
            "html_url": "https://github.com/acme/app/pull/7",
            "access_token": LEAK,
            "refresh_token": "refresh_PROXY_CREATE_PR",
        });
        let clean = sanitize_value(&dirty);
        assert!(!contains_secret_keys(&clean));
        let clean_s = serde_json::to_string(&clean).expect("serialize");
        assert!(
            !clean_s.contains(LEAK) && !clean_s.contains("refresh_PROXY"),
            "create_pr result must not echo the token: {clean_s}"
        );
        assert_eq!(clean["number"], 7);

        // Decrypt fail: wrong key, sealed token must not appear in the error.
        // invoke maps credentials_for Err → ok:false decrypt_failed with a
        // static summary (no `{e}` interpolation that could echo material).
        let seal_key = crate::CredentialsKey::from_secret("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .expect("seal key");
        let wrong_key = crate::CredentialsKey::from_secret("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .expect("wrong key");
        let sealed = seal_key
            .encrypt(&json!({ "access_token": LEAK }))
            .expect("seal");
        assert!(
            !serde_json::to_string(&sealed)
                .expect("sealed")
                .contains(LEAK),
            "sealed credentials must not contain the plaintext token"
        );
        let decrypt_err = wrong_key
            .decrypt(&sealed)
            .expect_err("wrong INTEGRATIONS_ENCRYPTION_KEY must fail")
            .to_string();
        assert!(
            !decrypt_err.contains(LEAK),
            "decrypt failure must not echo the token: {decrypt_err}"
        );

        // create_pr must be wired (not only listed). Missing owner/title is
        // bad_args locally — never unknown_op, never echo the bearer.
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("http");
        let missing = super::apps::dispatch(
            "github",
            &http,
            LEAK,
            "create_pr",
            &json!({ "access_token": LEAK, "token": LEAK }),
            &json!({}),
        )
        .await;
        assert!(
            !missing.ok,
            "create_pr bad args must be ok:false: {missing:?}"
        );
        assert_eq!(
            missing.error_code.as_deref(),
            Some("bad_args"),
            "create_pr must be dispatched (bad_args), not unknown_op: {missing:?}"
        );
        let missing_s = serde_json::to_string(&missing).expect("serialize missing");
        assert!(
            !missing_s.contains(LEAK),
            "create_pr bad_args must not echo the token: {missing_s}"
        );

        // Valid create_pr hits outbound HTTP. A local sink is not GitHub —
        // ok:false, not unknown_op, bearer never appears on the result.
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

        // Unknown op is fail-closed locally — never a GitHub CONNECT, never
        // treated as create_pr, never echoes a token stuffed in args.
        let unknown_sink = CreatePrSink::bind();
        let unknown_dispatch = super::apps::dispatch(
            "github",
            &unknown_sink.http,
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
            !unknown_dispatch.ok,
            "unknown github op must be ok:false: {unknown_dispatch:?}"
        );
        assert_eq!(
            unknown_dispatch.error_code.as_deref(),
            Some("unknown_op"),
            "unknown github op must not be forwarded as create_pr: {unknown_dispatch:?}"
        );
        assert_eq!(
            unknown_sink.hits(),
            0,
            "unknown op must not hit GitHub (sink saw {}): {unknown_dispatch:?}",
            unknown_sink.hits()
        );
        let unknown_dispatch_s =
            serde_json::to_string(&unknown_dispatch).expect("serialize unknown dispatch");
        assert!(
            !unknown_dispatch_s.contains(LEAK),
            "unknown_op dispatch must not echo the token: {unknown_dispatch_s}"
        );

        let pool = fleet_db::PgPool::connect_lazy("postgres://unused:unused@127.0.0.1:9/none")
            .expect("lazy pool");
        // Closed pool fails acquire immediately — create_pr is allowlisted so
        // it reaches find_connected; unknown_op must not.
        pool.close().await;
        let key = crate::CredentialsKey::from_secret("dev-test-encryption-key-32chars!!")
            .expect("test key");
        let unknown = invoke(
            &pool,
            &key,
            ProxyRequest {
                workspace_id: uuid::Uuid::new_v4(),
                provider: "github".into(),
                operation: "dump_access_token".into(),
                args: json!({
                    "access_token": LEAK,
                    "connection_id": uuid::Uuid::new_v4(),
                    "workspace_id": uuid::Uuid::new_v4(),
                }),
                caller: ProxyCaller::System,
            },
        )
        .await;
        assert!(!unknown.ok, "unknown op must be ok:false: {unknown:?}");
        assert_eq!(
            unknown.error_code.as_deref(),
            Some("unknown_op"),
            "unknown op must not be forwarded or treated as create_pr: {unknown:?}"
        );
        let unknown_s = serde_json::to_string(&unknown).expect("serialize unknown");
        assert!(
            !unknown_s.contains(LEAK),
            "unknown_op must not echo the token: {unknown_s}"
        );

        let create_pr = invoke(
            &pool,
            &key,
            ProxyRequest {
                workspace_id: uuid::Uuid::new_v4(),
                provider: "github".into(),
                operation: "create_pr".into(),
                args: json!({
                    "owner": "acme",
                    "repo": "app",
                    "title": "add README",
                    "head": "agent-fleet/changes",
                    "access_token": LEAK,
                    "connection_id": uuid::Uuid::new_v4(),
                    "workspace_id": uuid::Uuid::new_v4(),
                }),
                caller: ProxyCaller::System,
            },
        )
        .await;
        assert!(
            !create_pr.ok,
            "create_pr without a connection must be ok:false: {create_pr:?}"
        );
        assert_ne!(
            create_pr.error_code.as_deref(),
            Some("unknown_op"),
            "create_pr is allowlisted — must load the session workspace, not unknown_op: {create_pr:?}"
        );
        let create_s = serde_json::to_string(&create_pr).expect("serialize create_pr");
        assert!(
            !create_s.contains(LEAK),
            "create_pr invoke must not echo the token: {create_s}"
        );

        let src = production_src();
        let invoke_src = fn_src(src, "pub async fn invoke");
        let unknown_at = invoke_src
            .find("unknown_op")
            .expect("unknown op fail-closed");
        let decrypt_at = invoke_src
            .find("decrypt_failed")
            .expect("decrypt fail-closed");
        let find_at = invoke_src
            .find("find_connected")
            .expect("workspace-scoped load");
        let dispatch_at = invoke_src
            .find("apps::dispatch")
            .expect("allowlisted dispatch");
        assert!(
            unknown_at < find_at && unknown_at < dispatch_at,
            "unknown op must not hit GitHub:\n{invoke_src}"
        );
        assert!(
            decrypt_at < dispatch_at,
            "decrypt failure must be ok:false before dispatch:\n{invoke_src}"
        );
        let decrypt_arm: String = invoke_src[decrypt_at..]
            .lines()
            .take(8)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            decrypt_arm.contains("ProxyResult::err")
                || invoke_src[..decrypt_at].contains("ProxyResult::err"),
            "decrypt failure must return ProxyResult::err:\n{decrypt_arm}"
        );
        assert!(
            decrypt_arm.contains("failed to decrypt credentials")
                && !decrypt_arm.contains("{e}")
                && !decrypt_arm.contains("{e:?}"),
            "decrypt_failed summary must be a static string (interpolating the decrypt error would echo material):\n{decrypt_arm}"
        );
        assert!(
            invoke_src.contains("sanitize_value(&result.data)")
                && invoke_src.contains("drop(access_token)"),
            "token must be dropped; result sanitized:\n{invoke_src}"
        );
        // Lookup is ProxyRequest.workspace_id (inlined or via a helper).
        // `&req.args` is only legal as the dispatch payload, after load.
        let before_dispatch = &invoke_src[..dispatch_at];
        assert!(
            before_dispatch.contains("find_connected")
                && (before_dispatch.contains("req.workspace_id")
                    || src.contains("fn connection_scope")),
            "create_pr lookup is the session workspace, not args:\n{before_dispatch}"
        );
        assert!(
            !before_dispatch.contains("req.args") && !before_dispatch.contains("connection_id"),
            "args.connection_id / args.workspace_id must not retarget lookup:\n{before_dispatch}"
        );
        if src.contains("fn connection_scope") {
            let scope = fn_src(src, "fn connection_scope");
            assert!(
                scope.contains("req.workspace_id")
                    && !scope.contains("req.args")
                    && !scope.contains("connection_id"),
                "create_pr lookup is the session workspace, not args:\n{scope}"
            );
        }
    }

    /// Docs §7.3: unknown op / disconnected / decrypt failure → tool
    /// `ok: false` with a message. Results are sanitized (no token echo).
    ///
    /// `integration_query` decrypts the OAuth token on the control plane,
    /// dispatches an allowlisted op, and returns a sanitized payload. The
    /// token (and the `access_token` key) must never appear in the body —
    /// including when the model stuffed one into args.
    #[tokio::test]
    async fn integration_query_decrypts_server_side_and_sanitizes_tokens() {
        const LEAK: &str = "gho_IQ_SANITIZE_MUST_NOT_ECHO_c0ffee";
        let stuffed = json!({
            "access_token": LEAK,
            "refresh_token": LEAK,
            "token": LEAK,
            "owner": "acme",
            "repo": "app",
        });

        // Same shape `proxy_tool_result` returns to the model / ChatAction.
        fn integration_query_body(result: &ProxyResult) -> Value {
            json!({
                "ok": result.ok,
                "provider": result.provider,
                "operation": result.operation,
                "summary": result.summary,
                "data": result.data,
                "error_code": result.error_code,
                "provider_status": result.provider_status,
                "security": {
                    "tokens_in_sandbox": false,
                    "decrypt_on": "control_plane",
                },
                "hint": "Tokens stay on the control plane; this payload is sanitized.",
                "supported_providers": supported_providers(),
                "operations": operations_for(&result.provider),
            })
        }

        fn assert_no_token_echo(label: &str, body: &str) {
            assert!(
                !body.contains("access_token") && !body.contains(LEAK),
                "{label} must not contain access_token / the decrypted token: {body}"
            );
            assert!(
                !body.contains("refresh_token") && !body.contains("client_secret"),
                "{label} must not echo other credential keys: {body}"
            );
        }

        let pool = fleet_db::PgPool::connect_lazy("postgres://unused:unused@127.0.0.1:9/none")
            .expect("lazy pool");
        pool.close().await;
        let key = crate::CredentialsKey::from_secret("dev-test-encryption-key-32chars!!")
            .expect("test key");
        let workspace_id = uuid::Uuid::new_v4();

        // Unknown op — fail-closed locally. A token in args must not appear.
        let unknown = invoke(
            &pool,
            &key,
            ProxyRequest {
                workspace_id,
                provider: "github".into(),
                operation: "raw_http".into(),
                args: stuffed.clone(),
                caller: ProxyCaller::Chat {
                    user_id: uuid::Uuid::new_v4(),
                },
            },
        )
        .await;
        assert!(!unknown.ok, "unknown op must be ok:false: {unknown:?}");
        assert_eq!(
            unknown.error_code.as_deref(),
            Some("unknown_op"),
            "docs §7.3 unknown op must be unknown_op, not forwarded: {unknown:?}"
        );
        assert_eq!(unknown.data, Value::Null);
        let unknown_body = serde_json::to_string(&integration_query_body(&unknown))
            .expect("serialize unknown_op body");
        assert_no_token_echo("unknown_op integration_query body", &unknown_body);
        assert_no_token_echo(
            "unknown_op ProxyResult",
            &serde_json::to_string(&unknown).expect("serialize unknown"),
        );

        // Slack unknown op is the same contract (not github-only).
        let slack_unknown = invoke(
            &pool,
            &key,
            ProxyRequest {
                workspace_id,
                provider: "slack".into(),
                operation: "not_a_real_op".into(),
                args: stuffed.clone(),
                caller: ProxyCaller::System,
            },
        )
        .await;
        assert!(
            !slack_unknown.ok,
            "unknown slack op must be ok:false: {slack_unknown:?}"
        );
        assert_eq!(
            slack_unknown.error_code.as_deref(),
            Some("unknown_op"),
            "unknown slack op must be unknown_op: {slack_unknown:?}"
        );
        assert_no_token_echo(
            "slack unknown_op body",
            &serde_json::to_string(&integration_query_body(&slack_unknown))
                .expect("serialize slack unknown"),
        );

        // Disconnected: allowlisted `list_repos` is not unknown_op. A dead
        // pool cannot load a row (`internal`); a live empty workspace is
        // `not_connected`. Either way the tool is ok:false and must not echo.
        let disconnected = invoke(
            &pool,
            &key,
            ProxyRequest {
                workspace_id,
                provider: "github".into(),
                operation: "list_repos".into(),
                args: stuffed.clone(),
                caller: ProxyCaller::Chat {
                    user_id: uuid::Uuid::new_v4(),
                },
            },
        )
        .await;
        assert!(
            !disconnected.ok,
            "disconnected / unloadable list_repos must be ok:false: {disconnected:?}"
        );
        assert_ne!(
            disconnected.error_code.as_deref(),
            Some("unknown_op"),
            "list_repos is allowlisted — disconnected must not look like unknown_op: {disconnected:?}"
        );
        assert!(
            matches!(
                disconnected.error_code.as_deref(),
                Some("not_connected") | Some("internal")
            ),
            "disconnected list_repos is not_connected (or internal if the pool is dead): {disconnected:?}"
        );
        assert_eq!(disconnected.data, Value::Null);
        let disconnected_body = serde_json::to_string(&integration_query_body(&disconnected))
            .expect("serialize disconnected body");
        assert_no_token_echo("disconnected integration_query body", &disconnected_body);

        // Decrypt fail: wrong INTEGRATIONS_ENCRYPTION_KEY must not echo the
        // sealed token. invoke maps credentials_for Err → ok:false
        // decrypt_failed with a static summary (no `{e}` interpolation).
        let seal_key = crate::CredentialsKey::from_secret("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .expect("seal key");
        let wrong_key = crate::CredentialsKey::from_secret("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .expect("wrong key");
        let sealed = seal_key
            .encrypt(&json!({ "access_token": LEAK, "refresh_token": LEAK }))
            .expect("seal");
        let sealed_s = serde_json::to_string(&sealed).expect("sealed");
        assert!(
            !sealed_s.contains(LEAK) && !sealed_s.contains("access_token"),
            "sealed credentials must not contain the plaintext token / access_token key: {sealed_s}"
        );
        let decrypt_err = wrong_key
            .decrypt(&sealed)
            .expect_err("wrong INTEGRATIONS_ENCRYPTION_KEY must fail")
            .to_string();
        assert!(
            !decrypt_err.contains(LEAK),
            "decrypt failure must not echo the token: {decrypt_err}"
        );

        let decrypt_failed = ProxyResult::err(
            "github",
            "list_repos",
            "decrypt_failed",
            "failed to decrypt credentials (check INTEGRATIONS_ENCRYPTION_KEY)",
            None,
        );
        assert!(!decrypt_failed.ok);
        assert_eq!(decrypt_failed.error_code.as_deref(), Some("decrypt_failed"));
        let decrypt_body = serde_json::to_string(&integration_query_body(&decrypt_failed))
            .expect("serialize decrypt_failed body");
        assert_no_token_echo("decrypt_failed integration_query body", &decrypt_body);

        // Success path: provider JSON that echoes the bearer must be stripped
        // before it becomes the integration_query body.
        let dirty = json!({
            "items": [{
                "name": "acme/app",
                "access_token": LEAK,
                "refresh_token": "refresh_IQ_MUST_NOT_ECHO",
                "token": LEAK,
            }],
        });
        let clean = sanitize_value(&dirty);
        assert!(
            !contains_secret_keys(&clean),
            "sanitized data must drop secret keys: {clean}"
        );
        let mut ok = ProxyResult::ok_result("github", "list_repos", "ok", dirty, Some(200));
        ok.data = sanitize_value(&ok.data);
        if contains_secret_keys(&ok.data) {
            ok.data = Value::Null;
        }
        let ok_body =
            serde_json::to_string(&integration_query_body(&ok)).expect("serialize ok body");
        assert_no_token_echo("successful integration_query body", &ok_body);
        assert_eq!(ok.data["items"][0]["name"], "acme/app");

        let src = production_src();
        let invoke_src = fn_src(src, "pub async fn invoke");
        let unknown_at = invoke_src
            .find("unknown_op")
            .expect("unknown op fail-closed");
        let not_connected_at = invoke_src
            .find("not_connected")
            .expect("disconnected fail-closed");
        let decrypt_at = invoke_src
            .find("decrypt_failed")
            .expect("decrypt fail-closed");
        let find_at = invoke_src
            .find("find_connected")
            .expect("workspace-scoped load");
        let creds_at = invoke_src
            .find("credentials_for")
            .expect("server-side decrypt via credentials_for");
        let dispatch_at = invoke_src
            .find("apps::dispatch")
            .expect("allowlisted dispatch");
        assert!(
            unknown_at < find_at && unknown_at < dispatch_at,
            "unknown op must return before load / provider HTTP:\n{invoke_src}"
        );
        assert!(
            not_connected_at < dispatch_at,
            "disconnected must be ok:false before dispatch:\n{invoke_src}"
        );
        assert!(
            creds_at < dispatch_at && decrypt_at < dispatch_at,
            "decrypt happens on the control plane, before dispatch:\n{invoke_src}"
        );
        assert!(
            find_at < creds_at,
            "credentials_for must run after the workspace connection is loaded:\n{invoke_src}"
        );
        let decrypt_start = invoke_src[..decrypt_at]
            .rfind("Err(e)")
            .expect("credentials_for Err must map to decrypt_failed");
        let decrypt_arm: String = invoke_src[decrypt_start..]
            .lines()
            .take(16)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            decrypt_arm.contains("ProxyResult::err")
                && decrypt_arm.contains("decrypt_failed")
                && decrypt_arm.contains("failed to decrypt credentials")
                && !decrypt_arm.contains("{e}")
                && !decrypt_arm.contains("{e:?}"),
            "decrypt_failed summary must be a static string (interpolating the decrypt error would echo material):\n{decrypt_arm}"
        );
        assert!(
            invoke_src.contains("sanitize_value(&result.data)")
                && invoke_src.contains("drop(access_token)")
                && invoke_src.contains("contains_secret_keys"),
            "token must be dropped; result sanitized; leftover secret keys redacted:\n{invoke_src}"
        );

        // Chat `integration_query` must not decrypt itself — tokens stay in
        // the proxy. The tool payload advertises decrypt_on=control_plane.
        let chat_src = include_str!("../../../fleet-server/src/chat_tools.rs");
        let chat_src = match chat_src.find("#[cfg(test)]") {
            Some(i) => &chat_src[..i],
            None => chat_src,
        };
        let iq = fn_src(chat_src, "async fn integration_query");
        assert!(
            iq.contains("proxy_tool_result") && !iq.contains("credentials_for"),
            "integration_query must dispatch via the proxy (decrypt stays server-side):\n{iq}"
        );
        assert!(
            !iq.contains("access_token") && !iq.contains("refresh_token"),
            "integration_query must not copy credentials into the tool payload:\n{iq}"
        );
        let ptr = fn_src(chat_src, "async fn proxy_tool_result");
        assert!(
            ptr.contains("proxy::invoke"),
            "proxy_tool_result must call proxy::invoke:\n{ptr}"
        );
        assert!(
            !ptr.contains("access_token")
                && !ptr.contains("refresh_token")
                && !ptr.contains("credentials_for"),
            "proxy_tool_result must not copy credentials into the tool payload:\n{ptr}"
        );
        assert!(
            ptr.contains("decrypt_on")
                && ptr.contains("control_plane")
                && ptr.contains("tokens_in_sandbox")
                && ptr.contains("false"),
            "integration_query body must advertise control-plane decrypt, no sandbox tokens:\n{ptr}"
        );
        assert!(
            ptr.contains("result.data") && !ptr.contains("req.args"),
            "tool payload must use the sanitized ProxyResult, not the inbound args:\n{ptr}"
        );
    }
}
