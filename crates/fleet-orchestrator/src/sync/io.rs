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

    fn wired_rec() -> (std::sync::Arc<Rec>, IoBoardSync) {
        let rec = std::sync::Arc::new(Rec {
            calls: std::sync::Mutex::new(vec![]),
        });
        let sync = IoBoardSync::new(crate::sandbox::io::IntegrationHandle::new(rec.clone()));
        (rec, sync)
    }

    fn item(ws: uuid::Uuid, provider: &str, key: &str, status: &str) -> TaskStatusSync {
        TaskStatusSync {
            board_task_id: uuid::Uuid::new_v4(),
            project_id: ws,
            external_provider: Some(provider.into()),
            external_key: Some(key.into()),
            status: status.into(),
        }
    }

    #[tokio::test]
    async fn transitions_jira_on_done_success() {
        let (rec, sync) = wired_rec();
        let ws = uuid::Uuid::new_v4();
        sync.sync_task_status(item(ws, "jira", "WEB-1", "done_success"))
            .await
            .unwrap();
        let calls = rec.calls.lock().unwrap();
        let jira = calls
            .iter()
            .find(|(p, o, _)| p == "jira" && o == "transition_issue")
            .expect("jira.transition_issue");
        assert_eq!(jira.2["key"], "WEB-1");
        assert_eq!(jira.2["transition_id"], "31");
    }

    #[tokio::test]
    async fn sets_linear_completed_on_done_success() {
        let (rec, sync) = wired_rec();
        let ws = uuid::Uuid::new_v4();
        sync.sync_task_status(item(ws, "linear", "ENG-9", "done_success"))
            .await
            .unwrap();
        let calls = rec.calls.lock().unwrap();
        let lin = calls
            .iter()
            .find(|(p, o, _)| p == "linear" && o == "update_issue")
            .expect("linear.update_issue");
        assert_eq!(lin.2["id"], "iss-1");
        assert_eq!(lin.2["state_id"], "st-done");
    }

    #[tokio::test]
    async fn closes_github_issue_on_done_success() {
        let (rec, sync) = wired_rec();
        let ws = uuid::Uuid::new_v4();
        sync.sync_task_status(item(ws, "github", "acme/app#4", "done_success"))
            .await
            .unwrap();
        let calls = rec.calls.lock().unwrap();
        let gh = calls
            .iter()
            .find(|(p, o, _)| p == "github" && o == "update_issue")
            .expect("github.update_issue");
        assert_eq!(gh.2["owner"], "acme");
        assert_eq!(gh.2["repo"], "app");
        assert_eq!(gh.2["number"], 4);
        assert_eq!(gh.2["state"], "closed");
    }

    #[tokio::test]
    async fn closes_gitlab_issue_on_done_success() {
        let (rec, sync) = wired_rec();
        let ws = uuid::Uuid::new_v4();
        sync.sync_task_status(item(ws, "gitlab", "group/app#9", "done_success"))
            .await
            .unwrap();
        let calls = rec.calls.lock().unwrap();
        let gl = calls
            .iter()
            .find(|(p, o, _)| p == "gitlab" && o == "update_issue")
            .expect("gitlab.update_issue");
        assert_eq!(gl.2["project"], "group/app");
        assert_eq!(gl.2["iid"], 9);
        assert_eq!(gl.2["state"], "closed");
    }

    #[tokio::test]
    async fn skips_when_invoker_or_provider_or_key_blank() {
        let rec = std::sync::Arc::new(Rec {
            calls: std::sync::Mutex::new(vec![]),
        });
        let none = IoBoardSync::new(crate::sandbox::io::IntegrationHandle::none());
        let wired = IoBoardSync::new(crate::sandbox::io::IntegrationHandle::new(rec.clone()));
        let ws = uuid::Uuid::new_v4();
        none.sync_task_status(item(ws, "jira", "WEB-1", "done_success"))
            .await
            .unwrap();
        wired
            .sync_task_status(item(ws, "   ", "WEB-1", "done_success"))
            .await
            .unwrap();
        wired
            .sync_task_status(item(ws, "jira", "  ", "done_success"))
            .await
            .unwrap();
        assert!(rec.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn does_not_close_github_on_in_progress() {
        let (rec, sync) = wired_rec();
        let ws = uuid::Uuid::new_v4();
        sync.sync_task_status(item(ws, "github", "acme/app#4", "in_progress"))
            .await
            .unwrap();
        let calls = rec.calls.lock().unwrap();
        assert!(
            !calls
                .iter()
                .any(|(p, o, _)| p == "github" && o == "update_issue"),
            "{calls:?}"
        );
    }

    #[tokio::test]
    async fn github_key_drives_add_comment_owner_repo_number() {
        let (rec, sync) = wired_rec();
        let ws = uuid::Uuid::new_v4();
        sync.sync_task_status(item(ws, "github", "acme/app#12", "in_progress"))
            .await
            .unwrap();
        let calls = rec.calls.lock().unwrap();
        let gh = calls
            .iter()
            .find(|(p, o, _)| p == "github" && o == "add_comment")
            .expect("github.add_comment");
        assert_eq!(gh.2["owner"], "acme");
        assert_eq!(gh.2["repo"], "app");
        assert_eq!(gh.2["number"], 12);
    }

    #[tokio::test]
    async fn gitlab_key_drives_add_comment_project_iid() {
        let (rec, sync) = wired_rec();
        let ws = uuid::Uuid::new_v4();
        sync.sync_task_status(item(ws, "gitlab", "group/app#9", "in_progress"))
            .await
            .unwrap();
        let calls = rec.calls.lock().unwrap();
        let gl = calls
            .iter()
            .find(|(p, o, _)| p == "gitlab" && o == "add_comment")
            .expect("gitlab.add_comment");
        assert_eq!(gl.2["project"], "group/app");
        assert_eq!(gl.2["iid"], 9);
    }

    #[tokio::test]
    async fn github_unparseable_key_skips_invoke() {
        let (rec, sync) = wired_rec();
        let ws = uuid::Uuid::new_v4();
        sync.sync_task_status(item(ws, "github", "not-an-issue", "done_success"))
            .await
            .unwrap();
        let calls = rec.calls.lock().unwrap();
        assert!(
            !calls.iter().any(|(p, _, _)| p == "github"),
            "{calls:?}"
        );
    }

    #[tokio::test]
    async fn gitlab_nested_project_iid_drives_add_comment_and_close() {
        let (rec, sync) = wired_rec();
        let ws = uuid::Uuid::new_v4();
        sync.sync_task_status(item(ws, "gitlab", "group/sub/app#9", "done_success"))
            .await
            .unwrap();
        let calls = rec.calls.lock().unwrap();
        let comment = calls
            .iter()
            .find(|(p, o, _)| p == "gitlab" && o == "add_comment")
            .expect("gitlab.add_comment");
        assert_eq!(comment.2["project"], "group/sub/app");
        assert_eq!(comment.2["iid"], 9);
        let close = calls
            .iter()
            .find(|(p, o, _)| p == "gitlab" && o == "update_issue")
            .expect("gitlab.update_issue");
        assert_eq!(close.2["project"], "group/sub/app");
        assert_eq!(close.2["iid"], 9);
        assert_eq!(close.2["state"], "closed");
    }

    #[tokio::test]
    async fn github_key_missing_slash_or_digits_skips_invoke() {
        let (rec, sync) = wired_rec();
        let ws = uuid::Uuid::new_v4();
        sync.sync_task_status(item(ws, "github", "acme#12", "done_success"))
            .await
            .unwrap();
        sync.sync_task_status(item(ws, "github", "acme/app", "done_success"))
            .await
            .unwrap();
        sync.sync_task_status(item(ws, "github", "acme/app#x", "done_success"))
            .await
            .unwrap();
        let calls = rec.calls.lock().unwrap();
        assert!(
            !calls.iter().any(|(p, _, _)| p == "github"),
            "{calls:?}"
        );
    }

    #[tokio::test]
    async fn gitlab_key_without_hash_skips_invoke() {
        let (rec, sync) = wired_rec();
        let ws = uuid::Uuid::new_v4();
        sync.sync_task_status(item(ws, "gitlab", "group/app", "done_success"))
            .await
            .unwrap();
        let calls = rec.calls.lock().unwrap();
        assert!(
            !calls.iter().any(|(p, _, _)| p == "gitlab"),
            "{calls:?}"
        );
    }

    struct ListsRec {
        calls: std::sync::Mutex<Vec<(String, String, Value)>>,
        jira_transitions: Value,
        linear_states: Value,
    }

    #[async_trait]
    impl crate::sandbox::io::IntegrationInvoker for ListsRec {
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
                ("linear", "list_workflow_states") => self.linear_states.clone(),
                ("jira", "list_transitions") => self.jira_transitions.clone(),
                _ => json!({}),
            };
            crate::sandbox::io::IntegrationResult {
                ok: true,
                summary: format!("{provider}.{operation}"),
                data,
            }
        }
    }

    fn wired_lists(
        jira_transitions: Value,
        linear_states: Value,
    ) -> (std::sync::Arc<ListsRec>, IoBoardSync) {
        let rec = std::sync::Arc::new(ListsRec {
            calls: std::sync::Mutex::new(vec![]),
            jira_transitions,
            linear_states,
        });
        let sync = IoBoardSync::new(crate::sandbox::io::IntegrationHandle::new(rec.clone()));
        (rec, sync)
    }

    #[tokio::test]
    async fn in_progress_sync_picks_jira_in_progress_and_linear_started_without_closing_github() {
        let (rec, sync) = wired_lists(
            json!({
                "items": [
                    { "id": "11", "name": "To Do", "to": "To Do" },
                    { "id": "21", "name": "In Progress", "to": "In Progress" },
                    { "id": "31", "name": "Done", "to": "Done" }
                ]
            }),
            json!({
                "items": [
                    { "id": "s1", "name": "Todo", "type": "unstarted" },
                    { "id": "s2", "name": "In Progress", "type": "started" },
                    { "id": "s3", "name": "Done", "type": "completed" }
                ]
            }),
        );
        let ws = uuid::Uuid::new_v4();
        sync.sync_task_status(item(ws, "jira", "WEB-1", "in_progress"))
            .await
            .unwrap();
        sync.sync_task_status(item(ws, "linear", "ENG-9", "in_progress"))
            .await
            .unwrap();
        sync.sync_task_status(item(ws, "github", "acme/app#4", "in_progress"))
            .await
            .unwrap();
        let calls = rec.calls.lock().unwrap();
        let jira = calls
            .iter()
            .find(|(p, o, _)| p == "jira" && o == "transition_issue")
            .expect("jira.transition_issue");
        assert_eq!(jira.2["key"], "WEB-1");
        assert_eq!(jira.2["transition_id"], "21");
        let lin = calls
            .iter()
            .find(|(p, o, _)| p == "linear" && o == "update_issue")
            .expect("linear.update_issue");
        assert_eq!(lin.2["id"], "iss-1");
        assert_eq!(lin.2["state_id"], "s2");
        assert!(
            calls
                .iter()
                .any(|(p, o, _)| p == "github" && o == "add_comment"),
            "{calls:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|(p, o, _)| p == "github" && o == "update_issue"),
            "{calls:?}"
        );
    }

    #[tokio::test]
    async fn linear_state_id_falls_back_to_name_contains_when_type_missing() {
        let (rec, sync) = wired_lists(
            json!({ "items": [] }),
            json!({
                "items": [
                    { "id": "s1", "name": "Todo", "type": "unstarted" },
                    { "id": "s2", "name": "In Progress", "type": "started" },
                    { "id": "s9", "name": "Completed" }
                ]
            }),
        );
        let ws = uuid::Uuid::new_v4();
        sync.sync_task_status(item(ws, "linear", "ENG-9", "done_success"))
            .await
            .unwrap();
        let calls = rec.calls.lock().unwrap();
        let lin = calls
            .iter()
            .find(|(p, o, _)| p == "linear" && o == "update_issue")
            .expect("linear.update_issue");
        assert_eq!(lin.2["state_id"], "s9");
    }

    #[tokio::test]
    async fn done_failed_closes_github_and_gitlab_and_jiras_cancel_not_done() {
        let (rec, sync) = wired_lists(
            json!({
                "items": [
                    { "id": "11", "name": "To Do", "to": "To Do" },
                    { "id": "21", "name": "In Progress", "to": "In Progress" },
                    { "id": "31", "name": "Done", "to": "Done" },
                    { "id": "41", "name": "Cancel", "to": "Cancel" }
                ]
            }),
            json!({ "items": [] }),
        );
        let ws = uuid::Uuid::new_v4();
        sync.sync_task_status(item(ws, "jira", "WEB-1", "done_failed"))
            .await
            .unwrap();
        sync.sync_task_status(item(ws, "github", "acme/app#4", "done_failed"))
            .await
            .unwrap();
        sync.sync_task_status(item(ws, "gitlab", "group/app#9", "done_failed"))
            .await
            .unwrap();
        let calls = rec.calls.lock().unwrap();
        let jira = calls
            .iter()
            .find(|(p, o, _)| p == "jira" && o == "transition_issue")
            .expect("jira.transition_issue");
        assert_eq!(jira.2["transition_id"], "41");
        let gh = calls
            .iter()
            .find(|(p, o, _)| p == "github" && o == "update_issue")
            .expect("github.update_issue");
        assert_eq!(gh.2["state"], "closed");
        let gl = calls
            .iter()
            .find(|(p, o, _)| p == "gitlab" && o == "update_issue")
            .expect("gitlab.update_issue");
        assert_eq!(gl.2["state"], "closed");
    }
}
