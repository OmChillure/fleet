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

pub use apps::{operations_for, supported_providers};
pub use sanitize::{contains_secret_keys, sanitize_value};
pub use types::{ProxyCaller, ProxyRequest, ProxyResult, MAX_PROXY_DATA_BYTES};

use std::time::Duration;

use fleet_db::PgPool;
use serde_json::Value;

use crate::crypto::CredentialsKey;
use crate::credentials_for;

/// Invoke an allowlisted integration operation.
///
/// Tokens are decrypted here, used for the outbound call, and never included
/// in [`ProxyResult`].
pub async fn invoke(pool: &PgPool, key: &CredentialsKey, req: ProxyRequest) -> ProxyResult {
    let provider = req.provider.trim().to_lowercase();
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

    let conn = match fleet_db::repos::integrations::find_connected(pool, req.workspace_id, &provider)
        .await
    {
        Ok(Some(c)) => c,
        Ok(None) => {
            return ProxyResult::err(
                &provider,
                &operation,
                "not_connected",
                format!(
                    "{provider} is not connected — open Workspace → Integrations to connect it"
                ),
                None,
            );
        }
        Err(e) => {
            tracing::error!(
                workspace_id = %req.workspace_id,
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

    let creds = match credentials_for(pool, req.workspace_id, &provider, key).await {
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
                workspace_id = %req.workspace_id,
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
        workspace_id = %req.workspace_id,
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
            req.workspace_id,
            &provider,
            &result.summary,
        )
        .await
        {
            tracing::error!(
                workspace_id = %req.workspace_id,
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
        workspace_id = %req.workspace_id,
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
    let mut out = String::from(
        "\n\n## Integration context (control-plane fetch; no secrets)\n\n",
    );
    for r in results {
        out.push_str(&format!(
            "### {} `{}` — {}\n\n",
            r.provider, r.operation, r.summary
        ));
        if r.ok {
            let pretty =
                serde_json::to_string_pretty(&r.data).unwrap_or_else(|_| "{}".into());
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

    #[test]
    fn operations_listed() {
        assert!(operations_for("github").contains(&"list_prs"));
        assert!(operations_for("github").contains(&"commit_files"));
        assert!(operations_for("github").contains(&"get_file"));
        assert!(operations_for("gitlab").contains(&"list_mrs"));
        assert!(operations_for("gitlab").contains(&"commit_files"));
        assert!(operations_for("gitlab").contains(&"create_mr"));
        assert!(operations_for("jira").contains(&"get_issue"));
        assert!(operations_for("jira").contains(&"create_issue"));
        assert!(operations_for("linear").contains(&"list_issues"));
        assert!(operations_for("linear").contains(&"create_issue"));
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
}
