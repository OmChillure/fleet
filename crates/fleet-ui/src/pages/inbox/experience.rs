//! Per-app inbox chrome. Each provider matches how that product works.

use super::catalog::{InboxAppInfo, InboxKind};

pub struct Experience {
    pub accent: &'static str,
    pub wash: &'static str,
    pub mark: &'static str,
    pub item_word: &'static str,
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

fn wash_for(id: &str) -> &'static str {
    match id {
        "slack" => "bg-[#4A154B]/10",
        "teams" => "bg-[#6264A7]/10",
        "github" => "bg-white/5",
        "gitlab" => "bg-[#FC6D26]/10",
        "bitbucket" | "jira" => "bg-[#0052CC]/10",
        "linear" => "bg-[#5E6AD2]/10",
        "trello" => "bg-[#0079BF]/10",
        "asana" => "bg-[#F06A6A]/10",
        "notion" => "bg-white/5",
        "confluence" => "bg-[#172B4D]/20",
        _ => "bg-[var(--app-bg-elevated)]",
    }
}

fn mark_for(id: &str) -> &'static str {
    match id {
        "slack" => "text-[#E01E5A]",
        "teams" => "text-[#7B83EB]",
        "github" => "text-[var(--app-fg)]",
        "gitlab" => "text-[#FC6D26]",
        "bitbucket" | "jira" => "text-[#4C9AFF]",
        "linear" => "text-[#5E6AD2]",
        "trello" => "text-[#61BD4F]",
        "asana" => "text-[#F06A6A]",
        "notion" => "text-[var(--app-fg)]",
        "confluence" => "text-[#2684FF]",
        _ => "text-[var(--app-fg)]",
    }
}

pub fn experience(info: &InboxAppInfo) -> Experience {
    Experience {
        accent: accent_for(info.id),
        wash: wash_for(info.id),
        mark: mark_for(info.id),
        item_word: match info.kind {
            InboxKind::Ticket => match info.id {
                "trello" => "card",
                "asana" => "task",
                _ => "issue",
            },
            InboxKind::Message => "message",
            InboxKind::Page => "page",
        },
    }
}

/// owner/name from a typed value or a github.com / gitlab.com URL.
pub fn normalize_owner_name(raw: &str) -> Option<String> {
    let s = raw.trim().trim_end_matches('/');
    if s.is_empty() {
        return None;
    }
    let stripped = s
        .strip_prefix("https://github.com/")
        .or_else(|| s.strip_prefix("http://github.com/"))
        .or_else(|| s.strip_prefix("github.com/"))
        .or_else(|| s.strip_prefix("https://gitlab.com/"))
        .or_else(|| s.strip_prefix("http://gitlab.com/"))
        .or_else(|| s.strip_prefix("gitlab.com/"))
        .or_else(|| s.strip_prefix("https://bitbucket.org/"))
        .or_else(|| s.strip_prefix("http://bitbucket.org/"))
        .or_else(|| s.strip_prefix("bitbucket.org/"))
        .unwrap_or(s);
    let mut parts = stripped.split('/');
    let owner = parts.next()?.trim();
    let name = parts.next()?.split(|c| c == '#' || c == '?').next()?.trim();
    if owner.is_empty() || name.is_empty() {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

/// Slack channel names keep a leading #; raw IDs stay as-is.
pub fn normalize_channel(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if s.starts_with('#') {
        let name = s.trim_start_matches('#').trim();
        if name.is_empty() {
            None
        } else {
            Some(format!("#{name}"))
        }
    } else {
        Some(s.to_string())
    }
}

/// 32-char Notion id, with or without dashes, from a pasted URL.
pub fn normalize_notion_id(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let path = s.split(['?', '#']).next().unwrap_or(s);
    let last = path.rsplit('/').next().unwrap_or(path);
    let hex: String = last.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() >= 32 {
        Some(hex[hex.len() - 32..].to_string())
    } else if last.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') && last.len() >= 8 {
        Some(last.to_string())
    } else {
        None
    }
}

/// Trello short board id from trello.com/b/<id>/…
pub fn normalize_trello_board(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(rest) = s
        .split("trello.com/b/")
        .nth(1)
        .or_else(|| s.split("trello.com/board/").nth(1))
    {
        let id = rest.split('/').next()?.trim();
        if id.is_empty() {
            None
        } else {
            Some(id.to_string())
        }
    } else {
        Some(s.to_string())
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
        let apps = super::super::catalog::INBOX_APPS;
        assert_eq!(apps.iter().find(|a| a.id == "slack").unwrap().action, "Handle");
        assert_eq!(
            apps.iter().find(|a| a.id == "github").unwrap().action,
            "Start run"
        );
        assert_eq!(
            apps.iter().find(|a| a.id == "notion").unwrap().action,
            "Analyze"
        );
        assert_eq!(
            experience(apps.iter().find(|a| a.id == "trello").unwrap()).item_word,
            "card"
        );
    }

    #[test]
    fn paste_helpers_accept_product_urls() {
        assert_eq!(
            normalize_owner_name("https://github.com/acme/api/issues/12"),
            Some("acme/api".into())
        );
        assert_eq!(
            normalize_owner_name("gitlab.com/group/app"),
            Some("group/app".into())
        );
        assert_eq!(normalize_channel("#eng"), Some("#eng".into()));
        assert_eq!(normalize_channel("C0123456789"), Some("C0123456789".into()));
        assert_eq!(
            normalize_trello_board("https://trello.com/b/AbCdEfGh/sprint"),
            Some("AbCdEfGh".into())
        );
        let notion =
            normalize_notion_id("https://www.notion.so/acme/2942d3a0b1c24e0aa1b2c3d4e5f60718?v=1")
                .expect("notion");
        assert_eq!(notion, "2942d3a0b1c24e0aa1b2c3d4e5f60718");
    }
}
