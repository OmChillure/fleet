//! Run-scoped team chat so live agents can talk to each other.
//!
//! Durable log: `DATA_DIR/projects/{project}/runs/{run}/chat.jsonl`
//! Per-agent cursor: `…/inbox/{plan_task_id}.seq`
//! Human dump: `…/TEAM_CHAT.md` (copied into each live workdir).
//!
//! Isolated session dirs never see each other's files — this log is the
//! shared channel. Keys stay on the control plane; only text goes here.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_BODY_CHARS: usize = 4000;
pub const MAX_MESSAGES: usize = 500;
pub const DEFAULT_ASK_WAIT_SECS: u64 = 60;
pub const MAX_ASK_WAIT_SECS: u64 = 90;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamTalk {
    pub data_dir: PathBuf,
    pub project_id: Uuid,
    pub run_id: Uuid,
    pub me: String,
    pub me_profile: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamMessage {
    pub id: Uuid,
    pub seq: u32,
    pub ts: DateTime<Utc>,
    pub from: String,
    pub from_profile: String,
    /// `"*"` = everyone on the run; otherwise a plan_task_id.
    pub to: String,
    /// `say` | `ask` | `reply`
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<Uuid>,
    pub body: String,
}

impl TeamTalk {
    pub fn from_spawn(
        data_dir: &Path,
        project_id: Uuid,
        run_id: Uuid,
        me: impl Into<String>,
        me_profile: impl Into<String>,
    ) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            project_id,
            run_id,
            me: me.into(),
            me_profile: me_profile.into(),
        }
    }

    pub fn run_dir(&self) -> PathBuf {
        run_dir(&self.data_dir, self.project_id, self.run_id)
    }

    pub fn chat_path(&self) -> PathBuf {
        self.run_dir().join("chat.jsonl")
    }
}

pub fn run_dir(data_dir: &Path, project_id: Uuid, run_id: Uuid) -> PathBuf {
    data_dir
        .join("projects")
        .join(project_id.to_string())
        .join("runs")
        .join(run_id.to_string())
}

pub fn talk_file(work_dir: &Path) -> PathBuf {
    work_dir.join(".team.json")
}

pub async fn write_team_talk(work_dir: &Path, talk: &TeamTalk) {
    let _ = tokio::fs::create_dir_all(work_dir).await;
    if let Ok(s) = serde_json::to_string_pretty(talk) {
        let _ = tokio::fs::write(talk_file(work_dir), s).await;
    }
}

pub fn load_team_talk(work_dir: &Path) -> Option<TeamTalk> {
    let raw = std::fs::read_to_string(talk_file(work_dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn normalize_to(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "*" | "all" | "team" | "everyone" | "broadcast" => "*".into(),
        other => other.to_string(),
    }
}

pub fn is_for_me(msg: &TeamMessage, me: &str) -> bool {
    if msg.from == me {
        return false;
    }
    msg.to == "*" || msg.to.eq_ignore_ascii_case(me)
}

pub async fn post_message(
    talk: &TeamTalk,
    to: &str,
    kind: &str,
    body: &str,
    in_reply_to: Option<Uuid>,
) -> Result<TeamMessage, String> {
    let body = body.trim();
    if body.is_empty() {
        return Err("message body is empty".into());
    }
    let body = if body.chars().count() > MAX_BODY_CHARS {
        let cut: String = body.chars().take(MAX_BODY_CHARS).collect();
        format!("{cut}\n…(truncated)")
    } else {
        body.to_string()
    };
    let to = normalize_to(to);
    if talk.me.trim().is_empty() {
        return Err("sender id missing".into());
    }
    let kind = match kind.trim().to_ascii_lowercase().as_str() {
        "ask" => "ask",
        "reply" => "reply",
        _ => "say",
    };
    if kind == "ask" && to == "*" {
        return Err("ask_teammate needs a specific teammate id (not *)".into());
    }

    let dir = talk.run_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("chat dir: {e}"))?;

    let chat_path = talk.chat_path();
    let from = talk.me.clone();
    let from_profile = talk.me_profile.clone();
    let msg = with_chat_lock(&dir, move || {
        let existing = read_messages_sync(&chat_path)?;
        if existing.len() >= MAX_MESSAGES {
            return Err(format!("team chat full ({MAX_MESSAGES} messages)"));
        }
        let seq = existing.last().map(|m| m.seq).unwrap_or(0) + 1;
        let msg = TeamMessage {
            id: Uuid::now_v7(),
            seq,
            ts: Utc::now(),
            from,
            from_profile,
            to,
            kind: kind.into(),
            in_reply_to,
            body,
        };
        let mut line = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
        line.push('\n');
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&chat_path)
            .map_err(|e| format!("chat append: {e}"))?;
        f.write_all(line.as_bytes())
            .map_err(|e| format!("chat write: {e}"))?;
        Ok(msg)
    })
    .await?;

    rewrite_team_chat_md(talk).await;
    Ok(msg)
}

pub async fn list_messages(talk: &TeamTalk) -> Vec<TeamMessage> {
    read_messages_sync(&talk.chat_path()).unwrap_or_default()
}

pub async fn unread_for(talk: &TeamTalk) -> Vec<TeamMessage> {
    let cursor = load_cursor(talk);
    list_messages(talk)
        .await
        .into_iter()
        .filter(|m| m.seq > cursor && is_for_me(m, &talk.me))
        .collect()
}

pub async fn mark_seen(talk: &TeamTalk, seq: u32) {
    if seq == 0 {
        return;
    }
    let dir = talk.run_dir().join("inbox");
    let _ = tokio::fs::create_dir_all(&dir).await;
    let _ = tokio::fs::write(cursor_path(talk), format!("{seq}\n")).await;
}

pub async fn mark_seen_through(talk: &TeamTalk, msgs: &[TeamMessage]) {
    if let Some(last) = msgs.iter().map(|m| m.seq).max() {
        let cur = load_cursor(talk);
        if last > cur {
            mark_seen(talk, last).await;
        }
    }
}

pub async fn wait_for_reply(
    talk: &TeamTalk,
    ask_id: Uuid,
    from: &str,
    wait: Duration,
) -> Option<TeamMessage> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let found = list_messages(talk).await.into_iter().find(|m| {
            m.in_reply_to == Some(ask_id)
                && m.from.eq_ignore_ascii_case(from)
                && is_for_me(m, &talk.me)
        });
        if let Some(msg) = found {
            mark_seen_through(talk, std::slice::from_ref(&msg)).await;
            return Some(msg);
        }
        if tokio::time::Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

pub fn format_inbox(msgs: &[TeamMessage]) -> String {
    if msgs.is_empty() {
        return "No new messages.".into();
    }
    let mut out = String::from("# Messages from teammates\n\n");
    out.push_str("These arrived while you were working. If someone asked you a question, reply with `send_message` (set `reply_to` to their message id) or `ask_teammate`.\n\n");
    for m in msgs {
        let dest = if m.to == "*" {
            "team".to_string()
        } else {
            format!("you (`{}`)", m.to)
        };
        out.push_str(&format!(
            "### {} `{from}` (`{profile}`) → {dest}\n- id: `{id}`\n- seq: {seq}\n",
            m.kind,
            from = m.from,
            profile = m.from_profile,
            dest = dest,
            id = m.id,
            seq = m.seq,
        ));
        if let Some(r) = m.in_reply_to {
            out.push_str(&format!("- in reply to: `{r}`\n"));
        }
        out.push_str("\n");
        out.push_str(m.body.trim());
        out.push_str("\n\n");
    }
    out
}

pub fn render_team_chat_md(msgs: &[TeamMessage]) -> String {
    let mut out = String::from(
        "# Team chat\n\nLive channel for this run. Agents post with `send_message` / `ask_teammate`.\n\n",
    );
    if msgs.is_empty() {
        out.push_str("_No messages yet._\n");
        return out;
    }
    for m in msgs {
        let dest = if m.to == "*" {
            "*".to_string()
        } else {
            m.to.clone()
        };
        out.push_str(&format!(
            "- **{}** `{}` → `{}` _{}_",
            m.kind,
            m.from,
            dest,
            m.ts.format("%H:%M:%S")
        ));
        if let Some(r) = m.in_reply_to {
            out.push_str(&format!(" (re: `{r}`)"));
        }
        out.push('\n');
        for line in m.body.lines() {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
    }
    out
}

pub async fn rewrite_team_chat_md(talk: &TeamTalk) {
    let msgs = list_messages(talk).await;
    let md = render_team_chat_md(&msgs);
    let _ = tokio::fs::create_dir_all(talk.run_dir()).await;
    let _ = tokio::fs::write(talk.run_dir().join("TEAM_CHAT.md"), md).await;
}

/// Copy the run-level TEAM_CHAT.md into a live session workdir.
pub async fn copy_team_chat_into(work_dir: &Path, talk: &TeamTalk) {
    rewrite_team_chat_md(talk).await;
    let src = talk.run_dir().join("TEAM_CHAT.md");
    if let Ok(body) = tokio::fs::read_to_string(&src).await {
        let _ = tokio::fs::write(work_dir.join("TEAM_CHAT.md"), body).await;
    }
}

fn cursor_path(talk: &TeamTalk) -> PathBuf {
    talk.run_dir()
        .join("inbox")
        .join(format!("{}.seq", sanitize_id(&talk.me)))
}

fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn load_cursor(talk: &TeamTalk) -> u32 {
    std::fs::read_to_string(cursor_path(talk))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn read_messages_sync(path: &Path) -> Result<Vec<TeamMessage>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(m) = serde_json::from_str::<TeamMessage>(line) {
            out.push(m);
        }
    }
    Ok(out)
}

async fn with_chat_lock<T, F>(dir: &Path, f: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    let lock_path = dir.join(".lock");
    for i in 0..80 {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(_) => {
                let result = tokio::task::spawn_blocking(f)
                    .await
                    .map_err(|e| format!("chat lock join: {e}"))?;
                let _ = std::fs::remove_file(&lock_path);
                return result;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                tokio::time::sleep(Duration::from_millis(15 + i.min(20))).await;
            }
            Err(e) => return Err(format!("chat lock: {e}")),
        }
    }
    // Stale lock — steal it so a crashed writer cannot jam the run.
    let _ = std::fs::remove_file(&lock_path);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
    {
        Ok(_) => {
            let result = tokio::task::spawn_blocking(f)
                .await
                .map_err(|e| format!("chat lock join: {e}"))?;
            let _ = std::fs::remove_file(&lock_path);
            result
        }
        Err(e) => Err(format!("chat lock busy: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn talks(root: &Path) -> (TeamTalk, TeamTalk) {
        let project = Uuid::now_v7();
        let run = Uuid::now_v7();
        let a = TeamTalk::from_spawn(root, project, run, "t1", "backend");
        let b = TeamTalk::from_spawn(root, project, run, "t2", "frontend");
        (a, b)
    }

    #[tokio::test]
    async fn send_is_unread_for_target_not_sender() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = talks(dir.path());
        let msg = post_message(&a, "t2", "say", "API is POST /hello", None)
            .await
            .unwrap();
        assert_eq!(msg.from, "t1");
        assert_eq!(msg.to, "t2");
        assert!(unread_for(&a).await.is_empty());
        let inbox = unread_for(&b).await;
        assert_eq!(inbox.len(), 1);
        assert!(inbox[0].body.contains("POST /hello"));
        mark_seen_through(&b, &inbox).await;
        assert!(unread_for(&b).await.is_empty());
    }

    #[tokio::test]
    async fn broadcast_reaches_everyone_else() {
        let dir = tempfile::tempdir().unwrap();
        let project = Uuid::now_v7();
        let run = Uuid::now_v7();
        let a = TeamTalk::from_spawn(dir.path(), project, run, "t1", "be");
        let b = TeamTalk::from_spawn(dir.path(), project, run, "t2", "fe");
        let c = TeamTalk::from_spawn(dir.path(), project, run, "t3", "rev");
        post_message(&a, "*", "say", "shared types in types.ts", None)
            .await
            .unwrap();
        assert_eq!(unread_for(&b).await.len(), 1);
        assert_eq!(unread_for(&c).await.len(), 1);
        assert!(unread_for(&a).await.is_empty());
    }

    #[tokio::test]
    async fn ask_and_reply_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = talks(dir.path());
        let ask = post_message(&a, "t2", "ask", "what port?", None)
            .await
            .unwrap();
        assert_eq!(ask.kind, "ask");

        let waiter = {
            let a = a.clone();
            let id = ask.id;
            tokio::spawn(async move { wait_for_reply(&a, id, "t2", Duration::from_secs(2)).await })
        };
        tokio::time::sleep(Duration::from_millis(80)).await;
        post_message(&b, "t1", "reply", "port 8080", Some(ask.id))
            .await
            .unwrap();
        let reply = waiter.await.unwrap().expect("reply");
        assert!(reply.body.contains("8080"));
        assert_eq!(reply.in_reply_to, Some(ask.id));
    }

    #[test]
    fn normalize_to_aliases() {
        assert_eq!(normalize_to("ALL"), "*");
        assert_eq!(normalize_to("team"), "*");
        assert_eq!(normalize_to(" t2 "), "t2");
    }

    #[test]
    fn run_dir_is_project_and_run_scoped() {
        let root = Path::new("/data");
        let project = Uuid::now_v7();
        let run = Uuid::now_v7();
        let dir = run_dir(root, project, run);
        assert_eq!(
            dir,
            root.join("projects")
                .join(project.to_string())
                .join("runs")
                .join(run.to_string())
        );
        let other = run_dir(root, Uuid::now_v7(), run);
        assert_ne!(
            dir, other,
            "same run_id under another project must not share the chat dir"
        );
    }

    #[tokio::test]
    async fn distinct_projects_do_not_share_chat_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let run = Uuid::now_v7();
        let a = TeamTalk::from_spawn(dir.path(), Uuid::now_v7(), run, "t1", "be");
        let b = TeamTalk::from_spawn(dir.path(), Uuid::now_v7(), run, "human", "human");
        post_message(&a, "*", "say", "secret from project A", None)
            .await
            .unwrap();
        assert!(a.chat_path().is_file());
        assert!(
            !b.chat_path().is_file(),
            "posting on project A must not create chat.jsonl under another project"
        );
        assert!(list_messages(&b).await.is_empty());
    }

    #[test]
    fn format_inbox_includes_ask() {
        let msg = TeamMessage {
            id: Uuid::nil(),
            seq: 1,
            ts: Utc::now(),
            from: "t1".into(),
            from_profile: "backend".into(),
            to: "t2".into(),
            kind: "ask".into(),
            in_reply_to: None,
            body: "route?".into(),
        };
        let s = format_inbox(&[msg]);
        assert!(s.contains("route?"));
        assert!(s.contains("ask"));
        assert!(s.contains("`t1`"));
    }
}
