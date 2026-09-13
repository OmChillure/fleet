//! Per-app inbox chrome. Each provider matches how that product works.

use super::api::TicketRow;
use super::catalog::{InboxAppInfo, InboxKind};

pub struct Experience {
    pub accent: &'static str,
    pub item_word: &'static str,
    pub action: &'static str,
    pub empty: &'static str,
}

pub fn experience(info: &InboxAppInfo) -> Experience {
    match info.id {
        "slack" => Experience {
            accent: "border-[#4A154B]/40",
            item_word: "message",
            action: "Handle",
            empty: "Invite the Slack app into a channel (/invite @the-app), then Refresh. Slack returns not_in_channel until the bot is a member.",
        },
        "teams" => Experience {
            accent: "border-[#6264A7]/40",
            item_word: "message",
            action: "Handle",
            empty: "Add the Teams app to a team/channel, then Refresh.",
        },
        "github" => Experience {
            accent: "border-white/10",
            item_word: "issue",
            action: "Start run",
            empty: "Connect GitHub and grant the app the repos you care about, then Refresh. Filter with owner/name.",
        },
        "gitlab" => Experience {
            accent: "border-[#FC6D26]/40",
            item_word: "issue",
            action: "Start run",
            empty: "Connect GitLab. Filter with group/project, then Refresh.",
        },
        "bitbucket" => Experience {
            accent: "border-[#0052CC]/40",
            item_word: "issue",
            action: "Start run",
            empty: "Connect Bitbucket. Issues show as workspace/repo#id.",
        },
        "jira" => Experience {
            accent: "border-[#0052CC]/40",
            item_word: "issue",
            action: "Start run",
            empty: "Connect Jira. Set JQL (default: not Done) and Refresh.",
        },
        "linear" => Experience {
            accent: "border-[#5E6AD2]/40",
            item_word: "issue",
            action: "Start run",
            empty: "Connect Linear. Filter by team key (ENG) if you do not want every team.",
        },
        "trello" => Experience {
            accent: "border-[#0079BF]/40",
            item_word: "card",
            action: "Start run",
            empty: "Connect Trello. Pin board ids or Refresh to pull cards from every board the token can see.",
        },
        "asana" => Experience {
            accent: "border-[#F06A6A]/40",
            item_word: "task",
            action: "Start run",
            empty: "Connect Asana, then Refresh to pull open tasks.",
        },
        "notion" => Experience {
            accent: "border-white/15",
            item_word: "page",
            action: "Analyze",
            empty: "Share Notion databases with the connection, paste database IDs, then Refresh.",
        },
        "confluence" => Experience {
            accent: "border-[#172B4D]/40",
            item_word: "page",
            action: "Analyze",
            empty: "Connect Confluence, then Refresh to pull pages.",
        },
        _ => Experience {
            accent: "border-[var(--app-border)]",
            item_word: match info.kind {
                InboxKind::Ticket => "item",
                InboxKind::Message => "message",
                InboxKind::Page => "page",
            },
            action: "Finish",
            empty: "Connect this app under Integrations, then Refresh.",
        },
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
        "trello" | "asana" | "notion" | "confluence" => t
            .state
            .clone(),
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
        assert_eq!(experience(&super::super::catalog::INBOX_APPS.iter().find(|a| a.id == "slack").unwrap()).action, "Handle");
        assert_eq!(experience(&super::super::catalog::INBOX_APPS.iter().find(|a| a.id == "github").unwrap()).action, "Start run");
        assert_eq!(experience(&super::super::catalog::INBOX_APPS.iter().find(|a| a.id == "notion").unwrap()).action, "Analyze");
    }
}
