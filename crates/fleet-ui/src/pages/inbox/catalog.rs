//! Inbox apps. Keep in sync with `fleet-server` `INBOX_SOURCES`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InboxKind {
    Ticket,
    Message,
    Page,
}

#[derive(Clone, Copy)]
pub struct InboxAppInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: InboxKind,
    pub blurb: &'static str,
    pub filter_label: &'static str,
    pub filter_hint: &'static str,
}

pub const INBOX_APPS: &[InboxAppInfo] = &[
    InboxAppInfo {
        id: "github",
        name: "GitHub",
        kind: InboxKind::Ticket,
        blurb: "Open issues become work+review runs. Filter by owner/name repos. Grant the GitHub App those repos.",
        filter_label: "Repos (comma, owner/name)",
        filter_hint: "Empty = whatever the GitHub App can see.",
    },
    InboxAppInfo {
        id: "gitlab",
        name: "GitLab",
        kind: InboxKind::Ticket,
        blurb: "Open issues become work+review runs. Filter by group/name projects.",
        filter_label: "Projects (comma, group/name)",
        filter_hint: "Empty = projects the connected GitLab token can list.",
    },
    InboxAppInfo {
        id: "bitbucket",
        name: "Bitbucket",
        kind: InboxKind::Ticket,
        blurb: "Open issues become work+review runs.",
        filter_label: "Repos (optional)",
        filter_hint: "Leave empty unless you pin specific repos.",
    },
    InboxAppInfo {
        id: "jira",
        name: "Jira",
        kind: InboxKind::Ticket,
        blurb: "Issues matching JQL become work+review runs.",
        filter_label: "JQL",
        filter_hint: "Empty = the site default open-issue query.",
    },
    InboxAppInfo {
        id: "linear",
        name: "Linear",
        kind: InboxKind::Ticket,
        blurb: "Issues become work+review runs. Filter by team keys (ENG).",
        filter_label: "Teams (comma, ENG)",
        filter_hint: "Empty = teams the Linear token can see.",
    },
    InboxAppInfo {
        id: "trello",
        name: "Trello",
        kind: InboxKind::Ticket,
        blurb: "Cards become work+review runs. Filter by board ids.",
        filter_label: "Boards (comma, ids)",
        filter_hint: "Empty = boards the Trello token can see.",
    },
    InboxAppInfo {
        id: "asana",
        name: "Asana",
        kind: InboxKind::Ticket,
        blurb: "Tasks become work+review runs.",
        filter_label: "Projects (optional)",
        filter_hint: "Leave empty unless you pin projects.",
    },
    InboxAppInfo {
        id: "slack",
        name: "Slack",
        kind: InboxKind::Message,
        blurb: "Channel messages become one ops task. Invite the Slack app into each channel first — otherwise Slack returns not_in_channel.",
        filter_label: "Channels (comma, names or IDs)",
        filter_hint: "Empty = channels the bot is already in. /invite @the-app in Slack.",
    },
    InboxAppInfo {
        id: "teams",
        name: "Microsoft Teams",
        kind: InboxKind::Message,
        blurb: "Channel messages become one ops task.",
        filter_label: "Channels (optional)",
        filter_hint: "Empty = channels the Teams app can read.",
    },
    InboxAppInfo {
        id: "notion",
        name: "Notion",
        kind: InboxKind::Page,
        blurb: "Database pages become one analysis task. Share those databases with the Notion connection.",
        filter_label: "Databases (comma, IDs)",
        filter_hint: "Empty = databases the Notion connection can see.",
    },
    InboxAppInfo {
        id: "confluence",
        name: "Confluence",
        kind: InboxKind::Page,
        blurb: "Pages become one analysis task.",
        filter_label: "Spaces (optional)",
        filter_hint: "Empty = spaces the Confluence token can see.",
    },
];

pub fn inbox_app(id: &str) -> Option<&'static InboxAppInfo> {
    let id = id.trim().to_ascii_lowercase();
    INBOX_APPS.iter().find(|a| a.id == id)
}

pub fn is_inbox_app(id: &str) -> bool {
    inbox_app(id).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_and_github_are_distinct_inbox_apps() {
        let slack = inbox_app("Slack").expect("slack");
        let gh = inbox_app("github").expect("github");
        assert_eq!(slack.kind, InboxKind::Message);
        assert_eq!(gh.kind, InboxKind::Ticket);
        assert_ne!(slack.filter_label, gh.filter_label);
        assert!(is_inbox_app("jira"));
        assert!(!is_inbox_app("figma"));
    }
}
