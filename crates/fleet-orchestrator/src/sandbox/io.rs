//! Control-plane integration calls for workers and run write-back.
//!
//! Tokens stay in fleet-server (`proxy::invoke`). This crate only holds the
//! callback + the mapping from an inbox source → a reply operation.

use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

use crate::domain::{ExternalRef, PlanIo};

#[derive(Debug, Clone)]
pub struct IntegrationResult {
    pub ok: bool,
    pub summary: String,
    pub data: Value,
}

#[async_trait]
pub trait IntegrationInvoker: Send + Sync {
    async fn invoke(
        &self,
        workspace_id: Uuid,
        provider: String,
        operation: String,
        args: Value,
    ) -> IntegrationResult;
}

pub struct NoopIntegrationInvoker;

#[async_trait]
impl IntegrationInvoker for NoopIntegrationInvoker {
    async fn invoke(
        &self,
        _workspace_id: Uuid,
        provider: String,
        operation: String,
        _args: Value,
    ) -> IntegrationResult {
        IntegrationResult {
            ok: false,
            summary: format!("{provider}.{operation} skipped (no control-plane I/O wired)"),
            data: Value::Null,
        }
    }
}

/// Cloneable handle so [`super::SpawnRequest`] can stay `Debug`.
#[derive(Clone)]
pub struct IntegrationHandle(pub Option<Arc<dyn IntegrationInvoker>>);

impl std::fmt::Debug for IntegrationHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "IntegrationHandle(live)"
        } else {
            "IntegrationHandle(none)"
        })
    }
}

impl Default for IntegrationHandle {
    fn default() -> Self {
        Self::none()
    }
}

impl IntegrationHandle {
    pub fn none() -> Self {
        Self(None)
    }

    pub fn new(inv: Arc<dyn IntegrationInvoker>) -> Self {
        Self(Some(inv))
    }

    pub fn get(&self) -> Option<Arc<dyn IntegrationInvoker>> {
        self.0.clone()
    }
}

/// Fill `{{summary}}` / `{{run_id}}` / `{{status}}` in output args.
pub fn subst_output_args(args: &Value, summary: &str, run_id: &str, status: &str) -> Value {
    match args {
        Value::String(s) => Value::String(
            s.replace("{{summary}}", summary)
                .replace("{{run_id}}", run_id)
                .replace("{{status}}", status),
        ),
        Value::Array(arr) => Value::Array(
            arr.iter()
                .map(|v| subst_output_args(v, summary, run_id, status))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), subst_output_args(v, summary, run_id, status));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Default write-back so a Slack/Notion/ticket query gets a reply on that same item.
pub fn default_output_for_ref(ext: &ExternalRef, summary: &str) -> Option<PlanIo> {
    let provider = ext.provider.trim().to_ascii_lowercase();
    let key = ext.key.trim();
    if key.is_empty() {
        return None;
    }
    match provider.as_str() {
        "slack" => {
            let (channel, thread) = split_slack_key(key)?;
            let mut args = json!({ "channel": channel, "text": summary });
            if let Some(ts) = thread {
                args["thread_ts"] = json!(ts);
            }
            Some(PlanIo {
                provider,
                operation: "post_message".into(),
                args,
            })
        }
        "notion" => Some(PlanIo {
            provider,
            operation: "append_blocks".into(),
            args: json!({ "page_id": key, "text": summary }),
        }),
        "github" => {
            let (owner, repo, number) = parse_owner_repo_num(key)?;
            Some(PlanIo {
                provider,
                operation: "add_comment".into(),
                args: json!({
                    "owner": owner,
                    "repo": repo,
                    "number": number,
                    "body": summary
                }),
            })
        }
        "gitlab" => {
            let (project, iid) = parse_gitlab_key(key)?;
            Some(PlanIo {
                provider,
                operation: "add_comment".into(),
                args: json!({ "project": project, "iid": iid, "body": summary }),
            })
        }
        "jira" => Some(PlanIo {
            provider,
            operation: "add_comment".into(),
            args: json!({ "key": key, "body": summary }),
        }),
        "linear" => Some(PlanIo {
            provider,
            operation: "add_comment".into(),
            args: json!({ "issue_id": key, "body": summary }),
        }),
        "trello" => Some(PlanIo {
            provider,
            operation: "add_comment".into(),
            args: json!({ "card_id": key, "text": summary }),
        }),
        "asana" => Some(PlanIo {
            provider,
            operation: "add_comment".into(),
            args: json!({ "task_id": key, "text": summary }),
        }),
        "teams" => {
            let (chat_id, _) = key
                .split_once(':')
                .map(|(a, b)| (a, Some(b)))
                .unwrap_or((key, None));
            Some(PlanIo {
                provider,
                operation: "send_chat_message".into(),
                args: json!({ "chat_id": chat_id, "text": summary }),
            })
        }
        _ => None,
    }
}

pub fn split_slack_key(key: &str) -> Option<(String, Option<String>)> {
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    if let Some((ch, ts)) = key.split_once(':') {
        let ch = ch.trim();
        let ts = ts.trim();
        if ch.is_empty() {
            return None;
        }
        if ts.is_empty() {
            return Some((ch.to_string(), None));
        }
        return Some((ch.to_string(), Some(ts.to_string())));
    }
    Some((key.to_string(), None))
}

fn parse_owner_repo_num(key: &str) -> Option<(String, String, u64)> {
    let (repo, num) = key.split_once('#')?;
    let (owner, name) = repo.split_once('/')?;
    let n = num
        .trim()
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some((owner.to_string(), name.to_string(), n))
}

fn parse_gitlab_key(key: &str) -> Option<(String, u64)> {
    if let Some((project, iid)) = key.split_once('#') {
        let n = iid
            .trim()
            .split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse()
            .ok()?;
        if project.trim().is_empty() {
            return None;
        }
        return Some((project.trim().to_string(), n));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_thread_reply() {
        let io = default_output_for_ref(
            &ExternalRef {
                provider: "slack".into(),
                key: "C123:171.1".into(),
            },
            "done",
        )
        .unwrap();
        assert_eq!(io.operation, "post_message");
        assert_eq!(io.args["channel"], "C123");
        assert_eq!(io.args["thread_ts"], "171.1");
        assert_eq!(io.args["text"], "done");
    }

    #[test]
    fn notion_append() {
        let io = default_output_for_ref(
            &ExternalRef {
                provider: "notion".into(),
                key: "page-abc".into(),
            },
            "shipped",
        )
        .unwrap();
        assert_eq!(io.operation, "append_blocks");
        assert_eq!(io.args["page_id"], "page-abc");
        assert_eq!(io.args["text"], "shipped");
    }

    #[test]
    fn github_comment() {
        let io = default_output_for_ref(
            &ExternalRef {
                provider: "github".into(),
                key: "acme/app#12".into(),
            },
            "pr up",
        )
        .unwrap();
        assert_eq!(io.provider, "github");
        assert_eq!(io.operation, "add_comment");
        assert_eq!(io.args["owner"], "acme");
        assert_eq!(io.args["repo"], "app");
        assert_eq!(io.args["number"], 12);
        assert_eq!(io.args["body"], "pr up");
    }

    #[test]
    fn github_malformed_or_empty_key_returns_none() {
        for key in ["", "acme/app", "acme#12", "/app#12", "acme/#12", "acme/app#x"] {
            assert_eq!(
                default_output_for_ref(
                    &ExternalRef {
                        provider: "github".into(),
                        key: key.into(),
                    },
                    "pr up",
                ),
                None,
                "github key {key:?} should not map to a write-back",
            );
        }
    }

    #[test]
    fn gitlab_comment_maps_project_iid_and_body() {
        let io = default_output_for_ref(
            &ExternalRef {
                provider: "gitlab".into(),
                key: "group/app#9".into(),
            },
            "pr up",
        )
        .unwrap();
        assert_eq!(io.provider, "gitlab");
        assert_eq!(io.operation, "add_comment");
        assert_eq!(io.args["project"], "group/app");
        assert_eq!(io.args["iid"], 9);
        assert_eq!(io.args["body"], "pr up");
    }

    #[test]
    fn ticket_providers_default_to_add_comment() {
        let cases = [
            ("jira", "WEB-1", "add_comment", "key", "WEB-1", "body"),
            ("linear", "ENG-9", "add_comment", "issue_id", "ENG-9", "body"),
            ("trello", "card-1", "add_comment", "card_id", "card-1", "text"),
            ("asana", "task-1", "add_comment", "task_id", "task-1", "text"),
            (
                "teams",
                "chat:thread",
                "send_chat_message",
                "chat_id",
                "chat",
                "text",
            ),
        ];
        for (provider, key, operation, id_field, id_value, text_field) in cases {
            let io = default_output_for_ref(
                &ExternalRef {
                    provider: provider.into(),
                    key: key.into(),
                },
                "pr up",
            )
            .unwrap();
            assert_eq!(io.provider, provider);
            assert_eq!(io.operation, operation);
            assert_eq!(io.args[id_field], id_value);
            assert_eq!(io.args[text_field], "pr up");
        }
    }

    #[test]
    fn subst_templates() {
        let v = json!({"text": "run {{run_id}} {{status}}: {{summary}}"});
        let out = subst_output_args(&v, "ok", "r1", "completed");
        assert_eq!(out["text"], "run r1 completed: ok");
    }
}
