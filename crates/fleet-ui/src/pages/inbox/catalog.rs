//! Inbox apps. Keep in sync with `fleet-server` `INBOX_SOURCES`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InboxKind {
    Ticket,
    Message,
    Page,
}

/// How this app shows up in a software team's day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InboxGroup {
    Code,
    Board,
    Chat,
    Docs,
}

impl InboxGroup {
    pub fn title(self) -> &'static str {
        match self {
            Self::Code => "Source control",
            Self::Board => "Issue trackers",
            Self::Chat => "Chat",
            Self::Docs => "Docs",
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            Self::Code => {
                "Open issues become Fleet runs. Agents implement them and open a PR or MR."
            }
            Self::Board => {
                "Tickets and cards become a work + review run. Status stays on the board."
            }
            Self::Chat => {
                "One message becomes one ops job. Fleet replies in the channel when it finishes."
            }
            Self::Docs => {
                "A shared page becomes an analysis run — read-through, not a code change."
            }
        }
    }

    pub const ALL: [InboxGroup; 4] = [Self::Code, Self::Board, Self::Chat, Self::Docs];
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct InboxAppInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: InboxKind,
    pub group: InboxGroup,
    /// One sentence: what shows up here.
    pub blurb: &'static str,
    /// What this app is in the user's job.
    pub work: &'static str,
    /// What Fleet does with items from this app.
    pub fleet_does: &'static str,
    /// What lands back in the source app.
    pub outcome: &'static str,
    /// Three-step loop shown on the page.
    pub flow: &'static [&'static str],
    pub filter_label: &'static str,
    pub filter_placeholder: &'static str,
    /// Monitor filter is stored and applied on poll. False = no fake box.
    pub has_filter: bool,
    pub filter_save: &'static str,
    pub examples: &'static [&'static str],
    /// Numbered setup, first visit.
    pub steps: &'static [&'static str],
    pub hook_title: &'static str,
    pub hook_help: &'static str,
    pub empty: &'static str,
    pub action: &'static str,
}

pub const INBOX_APPS: &[InboxAppInfo] = &[
    InboxAppInfo {
        id: "github",
        name: "GitHub",
        kind: InboxKind::Ticket,
        group: InboxGroup::Code,
        blurb: "Open issues from your repos show up here. Starting one launches a Fleet run to work on it.",
        work: "Your team files bugs and feature work as GitHub issues on the repo that owns the code.",
        fleet_does: "Start run launches a coding agent plus a reviewer. They use the GitHub connection to read the issue, change the repo, and open a pull request.",
        outcome: "A PR on the issue’s repo, plus a comment back on the issue when the run finishes.",
        flow: &[
            "Issue is open on a repo the GitHub App can see",
            "Start run → coding + review agents",
            "PR opened; Fleet comments on the issue",
        ],
        filter_label: "Only these repositories (optional)",
        filter_placeholder: "acme/api, acme/web — or paste a github.com URL",
        has_filter: true,
        filter_save: "Save repos",
        examples: &["acme/api", "acme/web"],
        steps: &[
            "Connect GitHub on this page if the green Connected badge is not there yet.",
            "On GitHub.com open Settings → Applications → Agent Fleet (or your app name) and allow the repositories you want Fleet to see.",
            "On the board, check only the repositories you want. All checked means every repo the app can already see.",
            "Turn on automatic launch if new issues should start a run without you pressing the button.",
        ],
        hook_title: "Push issues here without waiting for Refresh",
        hook_help: "In each GitHub repo: Settings → Webhooks → Add webhook. Paste this URL, content type application/json, events “Issues”.",
        empty: "No open issues yet. Finish the steps above, then Refresh. If the list is still empty, the GitHub App may not have access to those repos.",
        action: "Start run",
    },
    InboxAppInfo {
        id: "gitea",
        name: "Gitea",
        kind: InboxKind::Ticket,
        group: InboxGroup::Code,
        blurb: "Open issues from a Gitea instance show up here.",
        work: "Gitea issues live next to the repository. Your team already tracks bugs and pull requests there.",
        fleet_does: "Start run launches coding + review. Agents use the Gitea connection to read the issue and open a pull request.",
        outcome: "A pull request on that repository, with a note back on the issue.",
        flow: &[
            "Issue is open on a repo the token can read",
            "Start run → coding + review agents",
            "Pull request opened on the repo",
        ],
        filter_label: "Only these repositories (optional)",
        filter_placeholder: "org/repo, other/app",
        has_filter: true,
        filter_save: "Save repos",
        examples: &["worms/game", "worms/docs"],
        steps: &[
            "Connect Gitea on this page. The instance URL is the one configured for this Fleet (GITEA_BASE_URL).",
            "The token must be allowed to read issues on the repositories you care about.",
            "Optional: list repos below as owner/name. Empty means every repo the token can list.",
            "Save repos, then Refresh. Start run launches a Fleet job for that issue.",
        ],
        hook_title: "Gitea issue webhook",
        hook_help: "In the Gitea repo: Settings → Webhooks → Add webhook. Paste this URL, set the secret to the token in the URL, content type application/json, and enable Issue events.",
        empty: "No open issues yet. Connect Gitea, check repo access, then Refresh.",
        action: "Start run",
    },
    InboxAppInfo {
        id: "gogs",
        name: "Gogs",
        kind: InboxKind::Ticket,
        group: InboxGroup::Code,
        blurb: "Open issues from a Gogs instance show up here.",
        work: "Gogs issues live next to the repository. Your team already tracks bugs there.",
        fleet_does: "Start run launches coding + review. Agents use the Gogs connection to read the issue and update the repo.",
        outcome: "A commit on that repository, with a note back on the issue.",
        flow: &[
            "Issue is open on a repo the token can read",
            "Start run → coding + review agents",
            "Changes committed on the repo",
        ],
        filter_label: "Only these repositories (optional)",
        filter_placeholder: "org/repo, other/app",
        has_filter: true,
        filter_save: "Save repos",
        examples: &["worms/game", "worms/docs"],
        steps: &[
            "Connect Gogs on this page. Sign in with the Gogs user Fleet should act as. The instance URL is GOGS_BASE_URL.",
            "The token must be allowed to read issues on the repositories you care about.",
            "Optional: list repos below as owner/name. Empty means every repo the token can list.",
            "Save repos, then Refresh. Start run launches a Fleet job for that issue.",
        ],
        hook_title: "Gogs issue webhook",
        hook_help: "In the Gogs repo: Settings → Webhooks → Add webhook. Paste this URL, set the secret to the token in the URL, content type application/json, and enable Issue events.",
        empty: "No open issues yet. Connect Gogs, check repo access, then Refresh.",
        action: "Start run",
    },
    InboxAppInfo {
        id: "gitlab",
        name: "GitLab",
        kind: InboxKind::Ticket,
        group: InboxGroup::Code,
        blurb: "Open issues from your GitLab projects show up here.",
        work: "GitLab issues live next to the project. Your team already tracks bugs and MRs there.",
        fleet_does: "Start run launches coding + review. Agents use the GitLab connection to read the issue and open a merge request.",
        outcome: "A merge request on that project, with a note back on the issue.",
        flow: &[
            "Issue is open on a project the token can read",
            "Start run → coding + review agents",
            "Merge request opened on the project",
        ],
        filter_label: "Only these projects (optional)",
        filter_placeholder: "group/project, other-group/app",
        has_filter: true,
        filter_save: "Save projects",
        examples: &["group/api", "group/web"],
        steps: &[
            "Connect GitLab on this page.",
            "The token must be allowed to read issues on the projects you care about.",
            "Optional: list projects below as group/name. Empty means every project the token can list.",
            "Save projects, then Refresh. Start run launches a Fleet job for that issue.",
        ],
        hook_title: "GitLab issue webhook",
        hook_help: "In the GitLab project: Settings → Webhooks. Paste this URL and enable Issue events.",
        empty: "No open issues yet. Connect GitLab, check project access, then Refresh.",
        action: "Start run",
    },
    InboxAppInfo {
        id: "bitbucket",
        name: "Bitbucket",
        kind: InboxKind::Ticket,
        group: InboxGroup::Code,
        blurb: "Open issues from Bitbucket repos show up here.",
        work: "Bitbucket Cloud holds the repo and its issue tracker. Fleet lists every repo the connection can see.",
        fleet_does: "Start run launches coding + review against that workspace/repo issue.",
        outcome: "Agents work the issue and can open a pull request on the same repo.",
        flow: &[
            "Issue is open on a Bitbucket repo",
            "Start run → coding + review agents",
            "Pull request on that workspace/repo",
        ],
        filter_label: "",
        filter_placeholder: "",
        has_filter: false,
        filter_save: "Save",
        examples: &[],
        steps: &[
            "Connect Bitbucket on this page with the Atlassian account that owns the workspace.",
            "Grant the connection access to the repositories whose issues you want Fleet to pick up.",
            "Refresh. Every open issue those repos expose is listed below.",
        ],
        hook_title: "Bitbucket issue webhook",
        hook_help: "In the Bitbucket repo: Repository settings → Webhooks. Add this URL for issue events.",
        empty: "No open issues yet. Connect Bitbucket and Refresh.",
        action: "Start run",
    },
    InboxAppInfo {
        id: "jira",
        name: "Jira",
        kind: InboxKind::Ticket,
        group: InboxGroup::Board,
        blurb: "Jira issues that match your search show up here.",
        work: "Jira is the sprint board. Engineers already live in JQL, issue keys like ENG-142, and status categories.",
        fleet_does: "Start run takes one issue and runs work + review. Agents can transition or comment through the Jira connection.",
        outcome: "The issue stays the source of truth. Fleet comments when the run finishes.",
        flow: &[
            "Issue matches your JQL (default: not Done)",
            "Start run → work + review",
            "Comment / status update back on the issue",
        ],
        filter_label: "Which issues? (Jira search)",
        filter_placeholder: "Leave empty for “not Done”. Or type JQL such as project = ENG AND status != Done",
        has_filter: true,
        filter_save: "Save search",
        examples: &[
            "statusCategory != Done ORDER BY updated DESC",
            "assignee = currentUser() AND statusCategory != Done",
            "project = ENG AND status != Done",
        ],
        steps: &[
            "Connect Jira on this page with the Atlassian site that holds the project.",
            "Leave the search empty to pull issues that are not Done. Or type the same JQL you use in Jira’s issue search bar.",
            "Save search, then Refresh. Start run launches a Fleet job for that issue.",
        ],
        hook_title: "Jira outgoing webhook",
        hook_help: "In Jira, add an outgoing webhook (or automation rule) that POSTs issue updates to this URL.",
        empty: "No matching issues. Connect Jira, adjust the search if you set one, then Refresh.",
        action: "Start run",
    },
    InboxAppInfo {
        id: "linear",
        name: "Linear",
        kind: InboxKind::Ticket,
        group: InboxGroup::Board,
        blurb: "Linear issues show up here (for example ENG-123).",
        work: "Linear issues are keyed by team (ENG-123). Cycles and teams are how the product team already plans.",
        fleet_does: "Start run launches work + review for that identifier. Agents can comment on the Linear issue.",
        outcome: "The Linear issue stays open as the tracker; Fleet reports back on it.",
        flow: &[
            "Issue is open on a team the token can see",
            "Start run → work + review",
            "Note posted on ENG-123",
        ],
        filter_label: "Only these teams (optional)",
        filter_placeholder: "ENG, DES",
        has_filter: true,
        filter_save: "Save teams",
        examples: &["ENG", "DES"],
        steps: &[
            "Connect Linear on this page.",
            "Optional: type team keys below (the letters on issues, like ENG). Empty means every team the token can see.",
            "Save teams, then Refresh.",
        ],
        hook_title: "Linear webhook",
        hook_help: "In Linear: Settings → API → Webhooks. Paste this URL.",
        empty: "No issues yet. Connect Linear and Refresh.",
        action: "Start run",
    },
    InboxAppInfo {
        id: "trello",
        name: "Trello",
        kind: InboxKind::Ticket,
        group: InboxGroup::Board,
        blurb: "Cards from your Trello boards show up here.",
        work: "Trello boards are the lightweight kanban. A card is the unit of work, not an issue key.",
        fleet_does: "Start run treats a card as a ticket: work + review. Agents can comment on the card.",
        outcome: "The card stays on the board. Fleet comments when the run finishes.",
        flow: &[
            "Card is open on a board the token can see",
            "Start run → work + review",
            "Comment on the card",
        ],
        filter_label: "Only these boards (optional)",
        filter_placeholder: "Board id from the board URL (trello.com/b/<id>/…)",
        has_filter: true,
        filter_save: "Save boards",
        examples: &[],
        steps: &[
            "Connect Trello on this page.",
            "Open a board in Trello. The URL looks like trello.com/b/AbCdEfGh/name — the AbCdEfGh part is the board id.",
            "Paste those ids below to limit which boards Fleet reads. Empty means every board the token can see.",
            "Save boards, then Refresh.",
        ],
        hook_title: "Trello board webhook",
        hook_help: "Add this URL as a Trello board webhook so new cards arrive without Refresh.",
        empty: "No cards yet. Connect Trello and Refresh.",
        action: "Start run",
    },
    InboxAppInfo {
        id: "asana",
        name: "Asana",
        kind: InboxKind::Ticket,
        group: InboxGroup::Board,
        blurb: "Open Asana tasks show up here.",
        work: "Asana tasks are assigned work across projects. Fleet lists incomplete tasks the token can see.",
        fleet_does: "Start run launches work + review for that task. Agents can comment on the Asana task.",
        outcome: "The task stays in Asana. Fleet comments when the run finishes.",
        flow: &[
            "Incomplete task in a workspace the token can see",
            "Start run → work + review",
            "Comment on the Asana task",
        ],
        filter_label: "",
        filter_placeholder: "",
        has_filter: false,
        filter_save: "Save",
        examples: &[],
        steps: &[
            "Connect Asana on this page with the workspace that holds the projects.",
            "Refresh to pull open tasks. Start run launches a Fleet job for that task.",
        ],
        hook_title: "Asana webhook",
        hook_help: "Add this URL as an Asana webhook if your plan supports it. Refresh is enough to start.",
        empty: "No tasks yet. Connect Asana and Refresh.",
        action: "Start run",
    },
    InboxAppInfo {
        id: "slack",
        name: "Slack",
        kind: InboxKind::Message,
        group: InboxGroup::Chat,
        blurb: "Messages from Slack channels the bot is in show up here. One message becomes one small job.",
        work: "Slack is where the team pings for help. A message in #eng is a request, not a ticket.",
        fleet_does: "Handle turns that message into one ops task. Fleet replies in the channel when the job finishes (it skips its own “Fleet run …” replies).",
        outcome: "A reply in the same channel — not a PR, unless the message asked for one.",
        flow: &[
            "Invite the Slack app into the channel",
            "Handle → one ops task",
            "Fleet replies in the channel",
        ],
        filter_label: "Only these channels (optional)",
        filter_placeholder: "#eng or C0123456789",
        has_filter: true,
        filter_save: "Save channels",
        examples: &["#eng", "#support"],
        steps: &[
            "Connect Slack on this page if you have not already.",
            "In Slack, open the channel you care about and type /invite @your-app-name (the name you gave the Slack app). Until the bot is a member, Slack will not let Fleet read or post there.",
            "Optional: list those channels below by name (#eng) or by channel ID. Leave the box empty to use every channel the bot is already in.",
            "Save channels, then Refresh. Handle turns one message into a Fleet job.",
        ],
        hook_title: "Slack Event Subscriptions",
        hook_help: "In the Slack app: Event Subscriptions → Request URL. Paste this URL so new messages arrive without Refresh. Subscribe to message.channels.",
        empty: "No messages yet. Invite the Slack app into a channel, then Refresh.",
        action: "Handle",
    },
    InboxAppInfo {
        id: "teams",
        name: "Microsoft Teams",
        kind: InboxKind::Message,
        group: InboxGroup::Chat,
        blurb: "Messages from Teams chats the app can read show up here.",
        work: "Teams is the Microsoft 365 chat. Fleet reads chats the connected account can already see.",
        fleet_does: "Handle turns one message into one ops task. Fleet can post back through the Teams connection.",
        outcome: "A reply in that chat — same idea as Slack, on Microsoft 365.",
        flow: &[
            "Connect the Microsoft account that sits in the team",
            "Handle → one ops task",
            "Reply posted in the chat",
        ],
        filter_label: "",
        filter_placeholder: "",
        has_filter: false,
        filter_save: "Save",
        examples: &[],
        steps: &[
            "Connect Microsoft Teams on this page with the work account that is a member of the team.",
            "Add the app to the team or chat you want Fleet to read, if your tenant requires it.",
            "Refresh. Messages from chats that account can list appear below.",
        ],
        hook_title: "Teams outgoing webhook",
        hook_help: "Point a Teams outgoing webhook at this URL if you want pushes instead of Refresh.",
        empty: "No messages yet. Add the Teams app to a chat, then Refresh.",
        action: "Handle",
    },
    InboxAppInfo {
        id: "notion",
        name: "Notion",
        kind: InboxKind::Page,
        group: InboxGroup::Docs,
        blurb: "Pages from shared Notion databases show up here. Analyze starts a read-through job.",
        work: "Specs, briefs, and meeting notes live in Notion databases. Fleet does not guess which pages — you share them with the connection.",
        fleet_does: "Analyze starts a read-through job (not a coding run). Agents read the page through the Notion connection.",
        outcome: "An analysis result. The page is the source; Fleet does not rewrite your wiki unless you ask in Chat.",
        flow: &[
            "Share the database with the Agent Fleet connection",
            "Analyze → one analysis task",
            "Read-through written back as a run result",
        ],
        filter_label: "Only these databases (optional)",
        filter_placeholder: "Database id from the Notion URL",
        has_filter: true,
        filter_save: "Save databases",
        examples: &[],
        steps: &[
            "Connect Notion on this page.",
            "In Notion, share each database with the Agent Fleet connection (same as sharing a page with a person).",
            "Optional: paste those database ids below (the 32-character id in the database URL). Empty means every database the connection can see.",
            "Save databases, then Refresh.",
        ],
        hook_title: "Refresh is enough",
        hook_help: "Notion does not always need a webhook. Refresh pulls shared pages. You can still paste this URL if you use a Notion automation.",
        empty: "No pages yet. Share a database with the Notion connection, then Refresh.",
        action: "Analyze",
    },
    InboxAppInfo {
        id: "confluence",
        name: "Confluence",
        kind: InboxKind::Page,
        group: InboxGroup::Docs,
        blurb: "Confluence pages show up here. Analyze starts a read-through job.",
        work: "Confluence spaces are the company wiki. Pages are the unit of knowledge, not tickets.",
        fleet_does: "Analyze starts a read-through of that page. Agents use the Confluence connection to fetch the body.",
        outcome: "An analysis run. The wiki page stays as-is unless you ask for an edit in Chat.",
        flow: &[
            "Page is in a space the token can view",
            "Analyze → one analysis task",
            "Read-through written as a run result",
        ],
        filter_label: "",
        filter_placeholder: "",
        has_filter: false,
        filter_save: "Save",
        examples: &[],
        steps: &[
            "Connect Confluence on this page with the Atlassian site that holds the spaces.",
            "The token must be allowed to view the spaces you care about.",
            "Refresh. Pages the token can search are listed below.",
        ],
        hook_title: "Confluence page webhook",
        hook_help: "Add this URL as a Confluence webhook for page events if you want pushes. Refresh is enough to start.",
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
            "{name} refused the request — the connection is missing a permission. Use Reconnect on this page and accept the extra access."
        );
    }
    if lower.contains("invalid_auth")
        || lower.contains("not_authed")
        || lower.contains("token_revoked")
        || lower.contains("account_inactive")
    {
        return format!(
            "The {name} sign-in is no longer valid. Use Reconnect on this page."
        );
    }
    if lower.contains("not_connected") {
        return format!("{name} is not connected yet. Use Connect {name} on this page.");
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
        assert_eq!(slack.group, InboxGroup::Chat);
        assert_eq!(gh.group, InboxGroup::Code);
        assert!(slack.has_filter);
        assert!(gh.has_filter);
        assert!(slack.steps.len() >= 3);
        assert!(gh.steps.len() >= 3);
        assert!(!is_inbox_app("figma"));
        // These providers have no monitor filter on poll — do not show a fake box.
        assert!(!inbox_app("bitbucket").expect("bb").has_filter);
        assert!(!inbox_app("teams").expect("teams").has_filter);
        assert!(!inbox_app("asana").expect("asana").has_filter);
        assert!(!inbox_app("confluence").expect("conf").has_filter);
        assert!(inbox_app("jira").expect("jira").examples.len() >= 2);
        for app in INBOX_APPS {
            assert_eq!(app.flow.len(), 3, "{} needs a 3-step loop", app.id);
            assert!(!app.work.is_empty(), "{}", app.id);
            assert!(!app.fleet_does.is_empty(), "{}", app.id);
            assert!(!app.outcome.is_empty(), "{}", app.id);
        }
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
