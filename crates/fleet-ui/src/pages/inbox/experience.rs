//! Per-app inbox chrome. Each provider matches how that product works.

use super::api::TicketRow;
use super::catalog::{InboxAppInfo, InboxKind};

pub struct Experience {
    pub accent: &'static str,
    pub item_word: &'static str,
    pub action: &'static str,
    pub empty: &'static str,
}

fn accent_for(id: &str) -> &'static str {
    match id {
        "slack" => "border-[#4A154B]/40",
        "teams" => "border-[#6264A7]/40",
        "github" => "border-white/10",
        "gitlab" => "border-[#FC6D26]/40",
        "bitbucket" | "jira" => "border-[#0052CC]/40",
        "linear" => "border-[#5E6AD2]/40",
        "trello" => "border-[#0079BF]/40",
        "asana" => "border-[#F06A6A]/40",
        "notion" => "border-white/15",
        "confluence" => "border-[#172B4D]/40",
        _ => "border-[var(--app-border)]",
    }
}

pub fn experience(info: &InboxAppInfo) -> Experience {
    Experience {
        accent: accent_for(info.id),
        item_word: match info.kind {
            InboxKind::Ticket => "issue",
            InboxKind::Message => "message",
            InboxKind::Page => "page",
        },
        action: info.action,
        empty: info.empty,
    }
}

/// Slack key is `C0123:1712345678.000100`.
pub fn slack_parts(key: &str) -> (String, String) {
    match key.split_once(':') {
        Some((ch, ts)) => (ch.to_string(), ts.to_string()),
        None => (key.to_string(), String::new()),
    }
}

/// GitHub / GitLab / Bitbucket key is `owner/repo#123`.
pub fn repo_issue_parts(key: &str) -> (String, String) {
    match key.rsplit_once('#') {
        Some((repo, n)) => (repo.to_string(), format!("#{n}")),
        None => (key.to_string(), String::new()),
    }
}

pub fn row_kicker(provider: &str, t: &TicketRow) -> String {
    match provider {
        "slack" | "teams" => {
            let (ch, ts) = slack_parts(&t.key);
            if ts.is_empty() {
                ch
            } else {
                format!("{ch} · {ts}")
            }
        }
        "github" | "gitlab" | "bitbucket" => {
            let (repo, num) = repo_issue_parts(&t.key);
            if num.is_empty() {
                repo
            } else {
                format!("{repo} {num}")
            }
        }
        "jira" | "linear" => t.key.clone(),
        "trello" | "asana" | "notion" | "confluence" => t.state.clone(),
        _ => t.key.clone(),
    }
}

pub fn row_title(provider: &str, t: &TicketRow) -> String {
    match provider {
        "slack" | "teams" => t.title.clone(),
        _ => t.title.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_and_github_parse_native_keys() {
        let (ch, ts) = slack_parts("C0BSHA7C51U:1712345678.000100");
        assert_eq!(ch, "C0BSHA7C51U");
        assert!(ts.starts_with("1712"));
        let (repo, num) = repo_issue_parts("acme/api#42");
        assert_eq!(repo, "acme/api");
        assert_eq!(num, "#42");
        assert_eq!(
            experience(
                &super::super::catalog::INBOX_APPS
                    .iter()
                    .find(|a| a.id == "slack")
                    .unwrap()
            )
            .action,
            "Handle"
        );
        assert_eq!(
            experience(
                &super::super::catalog::INBOX_APPS
                    .iter()
                    .find(|a| a.id == "github")
                    .unwrap()
            )
            .action,
            "Start run"
        );
        assert_eq!(
            experience(
                &super::super::catalog::INBOX_APPS
                    .iter()
                    .find(|a| a.id == "notion")
                    .unwrap()
            )
            .action,
            "Analyze"
        );
    }
}
