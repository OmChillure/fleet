//! Inbox apps. Keep in sync with `fleet-server` `INBOX_SOURCES`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InboxKind {
    Ticket,
    Message,
    Page,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct InboxAppInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: InboxKind,
    /// One sentence: what shows up here.
    pub blurb: &'static str,
    pub filter_label: &'static str,
    pub filter_placeholder: &'static str,
    /// Numbered setup, first visit.
    pub steps: &'static [&'static str],
    pub hook_help: &'static str,
    pub empty: &'static str,
    pub action: &'static str,
}

pub const INBOX_APPS: &[InboxAppInfo] = &[
    InboxAppInfo {
        id: "github",
        name: "GitHub",
        kind: InboxKind::Ticket,
        blurb: "Open issues from your repos show up here. Starting one launches a Fleet run to work on it.",
        filter_label: "Only these repositories (optional)",
        filter_placeholder: "acme/api, acme/web",
        steps: &[
            "Connect GitHub under Workspace → Integrations if the green Connected badge is not there yet.",
            "On GitHub.com open Settings → Applications → Agent Fleet (or your app name) and allow the repositories you want Fleet to see.",
            "If you only want some of those repos here, type them below as owner/name (for example acme/api). Leave the box empty to use every repo the app can already see.",
            "Click Save, then Refresh. Open issues appear below. Start run turns one issue into a Fleet job.",
        ],
        hook_help: "Optional, faster than Refresh: in each GitHub repo go to Settings → Webhooks → Add webhook. Paste this URL, content type JSON, events “Issues”.",
        empty: "No open issues yet. Finish the steps above, then Refresh. If the list is still empty, the GitHub App may not have access to those repos.",
        action: "Start run",
    },
    InboxAppInfo {
        id: "gitlab",
        name: "GitLab",
        kind: InboxKind::Ticket,
        blurb: "Open issues from your GitLab projects show up here.",
        filter_label: "Only these projects (optional)",
        filter_placeholder: "group/project, other-group/app",
        steps: &[
            "Connect GitLab under Workspace → Integrations.",
            "The token must be allowed to read issues on the projects you care about.",
            "Optional: list projects below as group/name. Empty means every project the token can list.",
            "Save, then Refresh. Start run launches a Fleet job for that issue.",
        ],
        hook_help: "Optional: in the GitLab project, Settings → Webhooks, paste this URL for Issue events.",
        empty: "No open issues yet. Connect GitLab, check project access, then Refresh.",
        action: "Start run",
    },
    InboxAppInfo {
        id: "bitbucket",
        name: "Bitbucket",
        kind: InboxKind::Ticket,
        blurb: "Open issues from Bitbucket repos show up here.",
        filter_label: "Only these repositories (optional)",
        filter_placeholder: "workspace/repo",
        steps: &[
            "Connect Bitbucket under Workspace → Integrations.",
            "Give the connection access to the repositories you want.",
            "Optional: list workspace/repo names below. Empty means every repo the token can see.",
            "Save, then Refresh.",
        ],
        hook_help: "Optional: add this URL as a Bitbucket repository webhook for issue events.",
        empty: "No open issues yet. Connect Bitbucket and Refresh.",
        action: "Start run",
    },
    InboxAppInfo {
        id: "jira",
        name: "Jira",
        kind: InboxKind::Ticket,
        blurb: "Jira issues that match your search show up here.",
        filter_label: "Which issues? (Jira search)",
        filter_placeholder: "Leave empty for “not Done”. Or type a Jira search such as project = ENG AND status != Done",
        steps: &[
            "Connect Jira under Workspace → Integrations.",
            "Below you can leave the box empty to pull issues that are not Done. If you know Jira search (JQL), type a query instead — same language you use in Jira’s issue search bar.",
            "Save, then Refresh. Start run launches a Fleet job for that issue.",
        ],
        hook_help: "Optional: in Jira, add an outgoing webhook to this URL for issue updates.",
        empty: "No matching issues. Connect Jira, adjust the search if you set one, then Refresh.",
        action: "Start run",
    },
    InboxAppInfo {
        id: "linear",
        name: "Linear",
        kind: InboxKind::Ticket,
        blurb: "Linear issues show up here (for example ENG-123).",
        filter_label: "Only these teams (optional)",
        filter_placeholder: "ENG, DES",
        steps: &[
            "Connect Linear under Workspace → Integrations.",
            "Optional: type team keys below (the letters on issues, like ENG). Empty means every team the token can see.",
            "Save, then Refresh.",
        ],
        hook_help: "Optional: in Linear, Settings → API → Webhooks, paste this URL.",
        empty: "No issues yet. Connect Linear and Refresh.",
        action: "Start run",
    },
    InboxAppInfo {
        id: "trello",
        name: "Trello",
        kind: InboxKind::Ticket,
        blurb: "Cards from your Trello boards show up here.",
        filter_label: "Only these boards (optional)",
        filter_placeholder: "Board ids from the board URL",
        steps: &[
            "Connect Trello under Workspace → Integrations.",
            "Optional: paste board ids (from the board URL) to limit which boards Fleet reads. Empty means every board the token can see.",
            "Save, then Refresh.",
        ],
        hook_help: "Optional: add this URL as a Trello board webhook.",
        empty: "No cards yet. Connect Trello and Refresh.",
        action: "Start run",
    },
    InboxAppInfo {
        id: "asana",
        name: "Asana",
        kind: InboxKind::Ticket,
        blurb: "Open Asana tasks show up here.",
        filter_label: "Only these projects (optional)",
        filter_placeholder: "Leave empty to use every project the token can see",
        steps: &[
            "Connect Asana under Workspace → Integrations.",
            "Refresh to pull open tasks. Start run launches a Fleet job for that task.",
        ],
        hook_help: "Optional: add this URL as an Asana webhook if your plan supports it.",
        empty: "No tasks yet. Connect Asana and Refresh.",
        action: "Start run",
    },
    InboxAppInfo {
        id: "slack",
        name: "Slack",
        kind: InboxKind::Message,
        blurb: "Messages from Slack channels the bot is in show up here. One message becomes one small job.",
        filter_label: "Only these channels (optional)",
        filter_placeholder: "#eng or C0123456789",
        steps: &[
            "Connect Slack under Workspace → Integrations if you have not already.",
            "In Slack, open the channel you care about and type /invite @your-app-name (the name you gave the Slack app). Until the bot is a member, Slack will not let Fleet read or post there.",
            "Optional: list those channels below by name (#eng) or by channel ID. Leave the box empty to use every channel the bot is already in.",
            "Save, then Refresh. Handle turns one message into a Fleet job.",
        ],
        hook_help: "Optional: in the Slack app, Event Subscriptions, paste this Request URL so new messages arrive without Refresh.",
        empty: "No messages yet. Invite the Slack app into a channel, then Refresh.",
        action: "Handle",
    },
    InboxAppInfo {
        id: "teams",
        name: "Microsoft Teams",
        kind: InboxKind::Message,
        blurb: "Messages from Teams channels the app can read show up here.",
        filter_label: "Only these channels (optional)",
        filter_placeholder: "Channel names or ids",
        steps: &[
            "Connect Microsoft Teams under Workspace → Integrations.",
            "Add the app to the team or channel you want Fleet to read.",
            "Optional: list those channels below. Empty means every channel the app can already see.",
            "Save, then Refresh.",
        ],
        hook_help: "Optional: point a Teams outgoing webhook at this URL.",
        empty: "No messages yet. Add the Teams app to a channel, then Refresh.",
        action: "Handle",
    },
    InboxAppInfo {
        id: "notion",
        name: "Notion",
        kind: InboxKind::Page,
        blurb: "Pages from shared Notion databases show up here. Analyze starts a read-through job.",
        filter_label: "Only these databases (optional)",
        filter_placeholder: "Database ids from the Notion URL",
        steps: &[
            "Connect Notion under Workspace → Integrations.",
            "In Notion, share each database with the Agent Fleet connection (same as sharing a page with a person).",
            "Optional: paste those database ids below (from the database URL). Empty means every database the connection can see.",
            "Save, then Refresh.",
        ],
        hook_help: "Notion does not always need a webhook. Refresh is enough for most workspaces.",
        empty: "No pages yet. Share a database with the Notion connection, then Refresh.",
        action: "Analyze",
    },
    InboxAppInfo {
        id: "confluence",
        name: "Confluence",
        kind: InboxKind::Page,
        blurb: "Confluence pages show up here. Analyze starts a read-through job.",
        filter_label: "Only these spaces (optional)",
        filter_placeholder: "SPACE keys",
        steps: &[
            "Connect Confluence under Workspace → Integrations.",
            "The token must be allowed to view the spaces you care about.",
            "Optional: list space keys below. Empty means every space the token can see.",
            "Save, then Refresh.",
        ],
        hook_help: "Optional: add this URL as a Confluence webhook for page events.",
        empty: "No pages yet. Connect Confluence and Refresh.",
        action: "Analyze",
    },
];

pub fn inbox_app(id: &str) -> Option<&'static InboxAppInfo> {
    let id = id.trim().to_ascii_lowercase();
    INBOX_APPS.iter().find(|a| a.id == id)
}

pub fn is_inbox_app(id: &str) -> bool {
    inbox_app(id).is_some()
}

/// Monitor last_error is all apps joined with "; ". Keep this app's lines only.
pub fn error_for_provider(joined: &str, provider: &str) -> Option<String> {
    let prefix = format!("{provider}:");
    let keep: Vec<&str> = joined
        .split(';')
        .map(str::trim)
        .filter(|line| line.to_ascii_lowercase().starts_with(&prefix))
        .collect();
    if keep.is_empty() {
        None
    } else {
        Some(keep.join("; "))
    }
}

/// Same filter, rewritten so a first-time user can act on it.
pub fn explain_error(joined: &str, provider: &str) -> Option<String> {
    let raw = error_for_provider(joined, provider)?;
    let parts: Vec<String> = raw
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| humanize_error(provider, s))
        .collect();
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn slack_channel_token(rest: &str) -> Option<&str> {
    let first = rest.split(':').next()?.trim();
    if first.len() >= 8
        && first.starts_with('C')
        && first.chars().all(|c| c.is_ascii_alphanumeric())
    {
        Some(first)
    } else {
        None
    }
}

fn humanize_error(provider: &str, raw: &str) -> String {
    let name = inbox_app(provider).map(|a| a.name).unwrap_or(provider);
    let prefix = format!("{provider}:");
    let rest = raw
        .strip_prefix(&prefix)
        .or_else(|| {
            let lower = raw.to_ascii_lowercase();
            let p = prefix.to_ascii_lowercase();
            lower
                .starts_with(&p)
                .then(|| raw.get(prefix.len()..).unwrap_or(raw))
        })
        .unwrap_or(raw)
        .trim();
    let lower = rest.to_ascii_lowercase();
    if lower.contains("not_in_channel") {
        return if let Some(ch) = slack_channel_token(rest) {
            format!(
                "The Slack app is not a member of channel {ch}. Open that channel in Slack and type /invite @your-app-name (the name you gave the Slack app)."
            )
        } else {
            "The Slack app is not a member of that channel. Open the channel in Slack and type /invite @your-app-name.".into()
        };
    }
    if lower.contains("channel_not_found") {
        return "Slack cannot find that channel. Check the name or ID, or invite the app into the channel first.".into();
    }
    if lower.contains("missing_scope") {
        return format!(
            "{name} refused the request — the connection is missing a permission. Disconnect and Connect again under Workspace → Integrations, and accept the extra access."
        );
    }
    if lower.contains("invalid_auth")
        || lower.contains("not_authed")
        || lower.contains("token_revoked")
        || lower.contains("account_inactive")
    {
        return format!(
            "The {name} connection is no longer valid. Disconnect and Connect again under Workspace → Integrations."
        );
    }
    if lower.contains("not_connected") {
        return format!(
            "{name} is not connected yet. Open Workspace → Integrations and connect it."
        );
    }
    let cleaned = rest
        .replace("Slack API:", "")
        .replace("GitHub API:", "")
        .replace("  ", " ");
    format!("{name}: {}", cleaned.trim())
}

/// Fill the webhook template. The API still sends `{provider}` as a placeholder.
pub fn hook_url_for(template: &str, provider: &str) -> String {
    template
        .replace("{provider}", provider)
        .replace("{{provider}}", provider)
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
        assert!(slack.steps.len() >= 3);
        assert!(gh.steps.len() >= 3);
        assert!(!is_inbox_app("figma"));
    }

    #[test]
    fn hook_url_fills_provider_not_braces() {
        let u = hook_url_for(
            "https://agentfleet.studio/api/integrations/webhooks/{provider}/abc",
            "slack",
        );
        assert!(u.contains("/webhooks/slack/"));
        assert!(!u.contains("{provider}"));
        let already = hook_url_for(&u, "slack");
        assert_eq!(already, u);
        let gh = hook_url_for(
            "https://agentfleet.studio/api/integrations/webhooks/{provider}/abc",
            "github",
        );
        assert!(gh.contains("/webhooks/github/"));
        assert!(!gh.contains("{provider}"));
    }

    #[test]
    fn slack_error_stays_off_github() {
        let err = "slack: C0BSHA7C51U: Slack API: not_in_channel";
        assert!(error_for_provider(err, "slack").is_some());
        assert!(error_for_provider(err, "github").is_none());
        assert!(explain_error(err, "github").is_none());
        let slack = explain_error(err, "slack").expect("slack");
        assert!(slack.contains("not a member of channel C0BSHA7C51U"));
        assert!(!slack.contains("not_in_channel"));
        assert!(!slack.contains("{provider}"));
    }
}
