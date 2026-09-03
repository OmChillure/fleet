//! Push fleet task status to Jira / Linear / GitHub / GitLab via the control-plane proxy.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::{ExternalBoardSync, SyncError, TaskStatusSync};
use crate::sandbox::io::IntegrationHandle;

pub struct IoBoardSync {
    pub io: IntegrationHandle,
}

impl IoBoardSync {
    pub fn new(io: IntegrationHandle) -> Self {
        Self { io }
    }
}

#[async_trait]
impl ExternalBoardSync for IoBoardSync {
    async fn sync_task_status(&self, item: TaskStatusSync) -> Result<(), SyncError> {
        let Some(inv) = self.io.get() else {
            return Ok(());
        };
        let Some(provider) = item
            .external_provider
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return Ok(());
        };
        let Some(key) = item
            .external_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return Ok(());
        };
        let ws = item.project_id;
        let body = format!(
            "Fleet task is now `{}` (board {}).",
            item.status, item.board_task_id
        );

        match provider {
            "jira" => {
                log_io(
                    inv.invoke(
                        ws,
                        "jira".into(),
                        "add_comment".into(),
                        json!({ "key": key, "body": body }),
                    )
                    .await,
                    "jira.add_comment",
                );
                if let Some(want) = jira_transition_hint(&item.status) {
                    let listed = inv
                        .invoke(
                            ws,
                            "jira".into(),
                            "list_transitions".into(),
                            json!({ "key": key }),
                        )
                        .await;
                    if listed.ok {
                        if let Some(tid) = pick_transition_id(&listed.data, want) {
                            log_io(
                                inv.invoke(
                                    ws,
                                    "jira".into(),
                                    "transition_issue".into(),
                                    json!({ "key": key, "transition_id": tid }),
                                )
                                .await,
                                "jira.transition_issue",
                            );
                        }
                    } else {
                        tracing::warn!(summary = %listed.summary, "jira.list_transitions failed");
                    }
                }
            }
            "linear" => {
                let issue = inv
                    .invoke(
                        ws,
                        "linear".into(),
                        "get_issue".into(),
                        json!({ "identifier": key }),
                    )
                    .await;
                let issue_id = issue
                    .ok
                    .then(|| {
                        issue
                            .data
                            .get("id")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    })
                    .flatten()
                    .unwrap_or_else(|| key.to_string());
                log_io(
                    inv.invoke(
                        ws,
                        "linear".into(),
                        "add_comment".into(),
                        json!({ "issue_id": issue_id, "body": body }),
                    )
                    .await,
                    "linear.add_comment",
                );
                if let Some(want) = linear_state_type(&item.status) {
                    let team_id = issue
                        .data
                        .get("team_id")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    if let Some(team_id) = team_id {
                        let listed = inv
                            .invoke(
                                ws,
                                "linear".into(),
                                "list_workflow_states".into(),
                                json!({ "team_id": team_id }),
                            )
                            .await;
                        if listed.ok {
                            if let Some(sid) = pick_linear_state_id(&listed.data, want) {
                                log_io(
                                    inv.invoke(
                                        ws,
                                        "linear".into(),
                                        "update_issue".into(),
                                        json!({ "id": issue_id, "state_id": sid }),
                                    )
                                    .await,
                                    "linear.update_issue",
                                );
                            }
                        } else {
                            tracing::warn!(
                                summary = %listed.summary,
                                "linear.list_workflow_states failed"
                            );
                        }
                    }
                }
            }
            "github" => {
                if let Some((owner, repo, number)) = parse_github_issue(key) {
                    log_io(
                        inv.invoke(
                            ws,
                            "github".into(),
                            "add_comment".into(),
                            json!({ "owner": owner, "repo": repo, "number": number, "body": body }),
                        )
                        .await,
                        "github.add_comment",
                    );
                    if let Some(state) = github_issue_state(&item.status) {
                        log_io(
                            inv.invoke(
                                ws,
                                "github".into(),
                                "update_issue".into(),
                                json!({
                                    "owner": owner,
                                    "repo": repo,
                                    "number": number,
                                    "state": state,
                                }),
                            )
                            .await,
                            "github.update_issue",
                        );
                    }
                }
            }
            "gitlab" => {
                if let Some((project, iid)) = parse_gitlab_issue(key) {
                    log_io(
                        inv.invoke(
                            ws,
                            "gitlab".into(),
                            "add_comment".into(),
                            json!({ "project": project, "iid": iid, "body": body }),
                        )
                        .await,
                        "gitlab.add_comment",
                    );
                    if let Some(state) = github_issue_state(&item.status) {
                        log_io(
                            inv.invoke(
                                ws,
                                "gitlab".into(),
                                "update_issue".into(),
                                json!({
                                    "project": project,
                                    "iid": iid,
                                    "state": state,
                                }),
                            )
                            .await,
                            "gitlab.update_issue",
                        );
                    }
                }
            }
            "bitbucket" => {
                if let Some((full, id)) = key.split_once('#') {
                    if let Some((workspace, repo)) = full.split_once('/') {
                        log_io(
                            inv.invoke(
                                ws,
                                "bitbucket".into(),
                                "add_comment".into(),
                                json!({
                                    "workspace": workspace,
                                    "repo": repo,
                                    "issue": id,
                                    "text": body,
                                }),
                            )
                            .await,
                            "bitbucket.add_comment",
                        );
                    }
                }
            }
            "trello" => {
                log_io(
                    inv.invoke(
                        ws,
                        "trello".into(),
                        "add_comment".into(),
                        json!({ "card_id": key, "text": body }),
                    )
                    .await,
                    "trello.add_comment",
                );
            }
            "asana" => {
                log_io(
                    inv.invoke(
                        ws,
                        "asana".into(),
                        "add_comment".into(),
                        json!({ "gid": key, "text": body }),
                    )
                    .await,
                    "asana.add_comment",
                );
            }
            "slack" => {
                if let Some((channel, ts)) = key.split_once(':') {
                    log_io(
                        inv.invoke(
                            ws,
                            "slack".into(),
                            "post_message".into(),
                            json!({
                                "channel": channel,
                                "text": body,
                                "thread_ts": ts,
                            }),
                        )
                        .await,
                        "slack.post_message",
                    );
                }
            }
            "teams" => {
                if let Some((chat_id, _)) = key.split_once(':') {
                    log_io(
                        inv.invoke(
                            ws,
                            "teams".into(),
                            "send_chat_message".into(),
                            json!({ "chat_id": chat_id, "text": body }),
                        )
                        .await,
                        "teams.send_chat_message",
                    );
                }
            }
            "notion" => {
                log_io(
                    inv.invoke(
                        ws,
                        "notion".into(),
                        "append_blocks".into(),
                        json!({ "page_id": key, "text": body }),
                    )
                    .await,
                    "notion.append_blocks",
                );
            }
            "confluence" => {
                log_io(
                    inv.invoke(
                        ws,
                        "confluence".into(),
                        "add_comment".into(),
                        json!({ "id": key, "text": body }),
                    )
                    .await,
                    "confluence.add_comment",
                );
            }
            other => {
                tracing::debug!(provider = other, "no board-sync adapter; skip");
            }
        }
        Ok(())
    }
}

fn log_io(res: crate::sandbox::io::IntegrationResult, op: &str) {
    if !res.ok {
        tracing::warn!(op, summary = %res.summary, "board sync call failed");
    }
}

fn jira_transition_hint(status: &str) -> Option<&'static str> {
    match status {
        "in_progress" | "assigned" => Some("progress"),
        "done_success" => Some("done"),
        "done_failed" | "cancelled" => Some("cancel"),
        "blocked" => Some("block"),
        _ => None,
    }
}

/// Linear workflow `type` for a fleet task status.
fn linear_state_type(status: &str) -> Option<&'static str> {
    match status {
        "in_progress" | "assigned" => Some("started"),
        "done_success" => Some("completed"),
        "done_failed" | "cancelled" => Some("canceled"),
        "blocked" => Some("started"),
        "queued" | "ready" => Some("unstarted"),
        _ => None,
    }
}

fn github_issue_state(status: &str) -> Option<&'static str> {
    match status {
        "done_success" | "done_failed" | "cancelled" => Some("closed"),
        _ => None,
    }
}

fn pick_linear_state_id(data: &Value, want_type: &str) -> Option<String> {
    let items = data.get("items")?.as_array()?;
    let want = want_type.to_ascii_lowercase();
    let by_type = items.iter().find_map(|t| {
        let ty = t
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ty == want {
            value_id(t.get("id"))
        } else {
            None
        }
    });
    if by_type.is_some() {
        return by_type;
    }
    items.iter().find_map(|t| {
        let name = t
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if name.contains(&want) {
            value_id(t.get("id"))
        } else {
            None
        }
    })
}

fn value_id(v: Option<&Value>) -> Option<String> {
    v.and_then(|v| {
        v.as_str()
            .map(|s| s.to_string())
            .or_else(|| v.as_i64().map(|n| n.to_string()))
    })
}

fn pick_transition_id(data: &Value, want: &str) -> Option<String> {
    let items = data.get("items")?.as_array()?;
    let want = want.to_ascii_lowercase();
    items.iter().find_map(|t| {
        let name = t
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let to = t
            .get("to")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if name.contains(&want) || to.contains(&want) {
            value_id(t.get("id"))
        } else {
            None
        }
    })
}

fn parse_github_issue(key: &str) -> Option<(String, String, u64)> {
    let (repo, num) = key.split_once('#')?;
    let (owner, name) = repo.split_once('/')?;
    let n = num
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()?;
    Some((owner.to_string(), name.to_string(), n))
}

fn parse_gitlab_issue(key: &str) -> Option<(String, u64)> {
    if let Some((proj, iid)) = key.split_once('#') {
        let n = iid
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .ok()?;
        return Some((proj.to_string(), n));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_github_and_gitlab_keys() {
        assert_eq!(
            parse_github_issue("acme/app#12"),
            Some(("acme".into(), "app".into(), 12))
        );
        assert_eq!(
            parse_gitlab_issue("group/app#9"),
            Some(("group/app".into(), 9))
        );
        assert!(parse_github_issue("not-an-issue").is_none());
    }

    #[test]
    fn picks_jira_and_linear_targets() {
        let jira = json!({
            "items": [
                { "id": "11", "name": "To Do", "to": "To Do" },
                { "id": "21", "name": "In Progress", "to": "In Progress" },
                { "id": "31", "name": "Done", "to": "Done" }
            ]
        });
        assert_eq!(pick_transition_id(&jira, "progress").as_deref(), Some("21"));
        assert_eq!(pick_transition_id(&jira, "done").as_deref(), Some("31"));

        let linear = json!({
            "items": [
                { "id": "s1", "name": "Todo", "type": "unstarted" },
                { "id": "s2", "name": "In Progress", "type": "started" },
                { "id": "s3", "name": "Done", "type": "completed" }
            ]
        });
        assert_eq!(
            pick_linear_state_id(&linear, "started").as_deref(),
            Some("s2")
        );
        assert_eq!(
            pick_linear_state_id(&linear, "completed").as_deref(),
            Some("s3")
        );
        assert_eq!(linear_state_type("done_success"), Some("completed"));
        assert_eq!(github_issue_state("done_success"), Some("closed"));
        assert_eq!(github_issue_state("in_progress"), None);
    }

    struct Rec {
        calls: std::sync::Mutex<Vec<(String, String, Value)>>,
    }

    #[async_trait]
    impl crate::sandbox::io::IntegrationInvoker for Rec {
        async fn invoke(
            &self,
            _workspace_id: uuid::Uuid,
            provider: String,
            operation: String,
            args: Value,
        ) -> crate::sandbox::io::IntegrationResult {
            self.calls
                .lock()
                .unwrap()
                .push((provider.clone(), operation.clone(), args.clone()));
            let data = match (provider.as_str(), operation.as_str()) {
                ("linear", "get_issue") => json!({
                    "id": "iss-1",
                    "team_id": "team-1",
                    "identifier": "ENG-9",
                }),
                ("linear", "list_workflow_states") => json!({
                    "items": [
                        { "id": "st-done", "name": "Done", "type": "completed" }
                    ]
                }),
                ("jira", "list_transitions") => json!({
                    "items": [{ "id": "31", "name": "Done", "to": "Done" }]
                }),
                _ => json!({}),
            };
            crate::sandbox::io::IntegrationResult {
                ok: true,
                summary: format!("{provider}.{operation}"),
                data,
            }
        }
    }

    #[tokio::test]
    async fn moves_jira_linear_github_cards() {
        let rec = std::sync::Arc::new(Rec {
            calls: std::sync::Mutex::new(vec![]),
        });
        let sync = IoBoardSync::new(crate::sandbox::io::IntegrationHandle::new(rec.clone()));
        let ws = uuid::Uuid::new_v4();
        for (provider, key) in [
            ("jira", "WEB-1"),
            ("linear", "ENG-9"),
            ("github", "acme/app#4"),
        ] {
            sync.sync_task_status(TaskStatusSync {
                board_task_id: uuid::Uuid::new_v4(),
                project_id: ws,
                external_provider: Some(provider.into()),
                external_key: Some(key.into()),
                status: "done_success".into(),
            })
            .await
            .unwrap();
        }
        let calls = rec.calls.lock().unwrap();
        let ops: Vec<String> = calls.iter().map(|(p, o, _)| format!("{p}.{o}")).collect();
        assert!(ops.contains(&"jira.transition_issue".into()), "{ops:?}");
        assert!(ops.contains(&"linear.update_issue".into()), "{ops:?}");
        assert!(ops.contains(&"github.update_issue".into()), "{ops:?}");
        let gh = calls
            .iter()
            .find(|(p, o, _)| p == "github" && o == "update_issue")
            .unwrap();
        assert_eq!(gh.2["state"], "closed");
        let lin = calls
            .iter()
            .find(|(p, o, _)| p == "linear" && o == "update_issue")
            .unwrap();
        assert_eq!(lin.2["state_id"], "st-done");
    }
}
