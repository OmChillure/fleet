//! Inbox adapters: which connected apps feed the watcher.

use serde_json::{json, Value};

/// What kind of work an inbox item is. Drives the auto-launch plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxKind {
    /// Issue / card / task → implement + review.
    Ticket,
    /// Chat message → one triage/ops task.
    Message,
    /// Doc / page → one handle/ops task.
    Page,
}

#[derive(Debug, Clone, Copy)]
pub struct InboxSource {
    pub provider: &'static str,
    pub kind: InboxKind,
}

pub const INBOX_SOURCES: &[InboxSource] = &[
    InboxSource {
        provider: "github",
        kind: InboxKind::Ticket,
    },
    InboxSource {
        provider: "gitlab",
        kind: InboxKind::Ticket,
    },
    InboxSource {
        provider: "gitea",
        kind: InboxKind::Ticket,
    },
    InboxSource {
        provider: "gogs",
        kind: InboxKind::Ticket,
    },
    InboxSource {
        provider: "bitbucket",
        kind: InboxKind::Ticket,
    },
    InboxSource {
        provider: "jira",
        kind: InboxKind::Ticket,
    },
    InboxSource {
        provider: "linear",
        kind: InboxKind::Ticket,
    },
    InboxSource {
        provider: "trello",
        kind: InboxKind::Ticket,
    },
    InboxSource {
        provider: "asana",
        kind: InboxKind::Ticket,
    },
    InboxSource {
        provider: "slack",
        kind: InboxKind::Message,
    },
    InboxSource {
        provider: "teams",
        kind: InboxKind::Message,
    },
    InboxSource {
        provider: "notion",
        kind: InboxKind::Page,
    },
    InboxSource {
        provider: "confluence",
        kind: InboxKind::Page,
    },
];

pub const DEFAULT_LAUNCH_LIMIT: i32 = 8;
pub const DEFAULT_POLL_LIMIT: i32 = 20;

pub fn is_inbox_provider(provider: &str) -> bool {
    let p = provider.trim().to_ascii_lowercase();
    INBOX_SOURCES.iter().any(|s| s.provider == p)
}

pub fn source_for(provider: &str) -> Option<&'static InboxSource> {
    let p = provider.trim().to_ascii_lowercase();
    INBOX_SOURCES.iter().find(|s| s.provider == p)
}

pub fn kind_for(provider: &str) -> InboxKind {
    source_for(provider)
        .map(|s| s.kind)
        .unwrap_or(InboxKind::Ticket)
}

/// Live set the watcher will poll this tick.
///
/// `follow_connected` (default): every currently connected inbox app.
/// Otherwise only the configured allowlist, still skipped if disconnected.
pub fn resolve_watching(
    follow_connected: bool,
    allowlist: &[String],
    connected: &[String],
) -> Vec<String> {
    let connected: Vec<String> = connected
        .iter()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| is_inbox_provider(s))
        .collect();
    let allow: Vec<String> = allowlist
        .iter()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| is_inbox_provider(s))
        .collect();

    let mut out = if follow_connected {
        connected
    } else {
        connected
            .into_iter()
            .filter(|p| allow.iter().any(|a| a == p))
            .collect()
    };
    out.sort();
    out.dedup();
    out
}

pub fn clamp_launch_limit(n: i32) -> i32 {
    n.clamp(1, 50)
}

pub fn clamp_poll_limit(n: i32) -> i32 {
    n.clamp(1, 100)
}

pub fn target_limit(poll_limit: i32) -> usize {
    ((poll_limit / 2).clamp(1, 16)) as usize
}

/// Merge a patch into stored filters. Legacy github_repos / jira_jql / … win
/// for their provider so existing UI and chat tools keep working.
pub fn merge_filters(
    existing: &Value,
    incoming: Option<&Value>,
    github_repos: Option<&[String]>,
    jira_jql: Option<&str>,
    slack_channels: Option<&[String]>,
    notion_databases: Option<&[String]>,
    gitlab_projects: Option<&[String]>,
    linear_teams: Option<&[String]>,
    trello_boards: Option<&[String]>,
) -> Value {
    let mut obj = existing.as_object().cloned().unwrap_or_default();
    if let Some(inc) = incoming.and_then(|v| v.as_object()) {
        for (k, v) in inc {
            let key = k.trim().to_ascii_lowercase();
            if is_inbox_provider(&key) {
                obj.insert(key, v.clone());
            }
        }
    }
    if let Some(repos) = github_repos {
        let repos: Vec<String> = repos
            .iter()
            .map(|r| r.trim().to_string())
            .filter(|r| r.contains('/'))
            .collect();
        obj.insert("github".into(), json!({ "repos": repos }));
    }
    if let Some(jql) = jira_jql {
        let t = jql.trim();
        if t.is_empty() {
            obj.remove("jira");
        } else {
            obj.insert("jira".into(), json!({ "jql": t }));
        }
    }
    if let Some(ch) = slack_channels {
        let ch: Vec<String> = ch
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        obj.insert("slack".into(), json!({ "channels": ch }));
    }
    if let Some(dbs) = notion_databases {
        let dbs: Vec<String> = dbs
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        obj.insert("notion".into(), json!({ "databases": dbs }));
    }
    if let Some(projects) = gitlab_projects {
        let projects: Vec<String> = projects
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        obj.insert("gitlab".into(), json!({ "projects": projects }));
    }
    if let Some(teams) = linear_teams {
        let teams: Vec<String> = teams
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        obj.insert("linear".into(), json!({ "teams": teams }));
    }
    if let Some(boards) = trello_boards {
        let boards: Vec<String> = boards
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        obj.insert("trello".into(), json!({ "boards": boards }));
    }
    Value::Object(obj)
}

pub fn string_list(filters: &Value, provider: &str, key: &str) -> Vec<String> {
    filters
        .get(provider)
        .and_then(|v| v.get(key))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

pub fn string_opt(filters: &Value, provider: &str, key: &str) -> Option<String> {
    filters
        .get(provider)
        .and_then(|v| v.get(key))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

pub fn github_repos(filters: &Value) -> Vec<String> {
    string_list(filters, "github", "repos")
}

pub fn jira_jql(filters: &Value) -> Option<String> {
    string_opt(filters, "jira", "jql")
}

pub fn slack_channels(filters: &Value) -> Vec<String> {
    string_list(filters, "slack", "channels")
}

pub fn notion_databases(filters: &Value) -> Vec<String> {
    string_list(filters, "notion", "databases")
}

pub fn gitlab_projects(filters: &Value) -> Vec<String> {
    string_list(filters, "gitlab", "projects")
}

pub fn gitea_repos(filters: &Value) -> Vec<String> {
    string_list(filters, "gitea", "repos")
}

pub fn gogs_repos(filters: &Value) -> Vec<String> {
    string_list(filters, "gogs", "repos")
}

pub fn linear_teams(filters: &Value) -> Vec<String> {
    string_list(filters, "linear", "teams")
}

pub fn trello_boards(filters: &Value) -> Vec<String> {
    string_list(filters, "trello", "boards")
}

pub fn listed_hit(allow: &[String], value: &str) -> bool {
    if allow.is_empty() {
        return true;
    }
    let v = value.trim();
    if v.is_empty() {
        return false;
    }
    allow
        .iter()
        .any(|raw| !raw.trim().is_empty() && raw.trim().eq_ignore_ascii_case(v))
}

/// Linear issue `ENG-123` matches team `ENG`, not a prefix of another team key.
fn linear_team_hit(allow: &[String], key: &str) -> bool {
    if allow.is_empty() {
        return true;
    }
    let key = key.trim();
    if key.is_empty() {
        return false;
    }
    allow.iter().any(|raw| {
        let team = raw.trim();
        if team.is_empty() {
            return false;
        }
        if key.eq_ignore_ascii_case(team) {
            return true;
        }
        let (head, rest) = match key.split_once('-') {
            Some(parts) => parts,
            None => return false,
        };
        head.eq_ignore_ascii_case(team)
            && !rest.is_empty()
            && rest.chars().all(|c| c.is_ascii_digit())
    })
}

/// Webhook ingest: a pinned allowlist accepts only those providers.
/// `follow_connected` no longer means "any inbox provider" — the path
/// provider must be listed (or the allowlist empty *and* a later HMAC /
/// connection check applies). Empty allowlist + follow_connected still
/// requires `is_inbox_provider` so `/jira/{token}` cannot stand in for GitHub.
pub fn webhook_provider_allowed(
    follow_connected: bool,
    allowlist: &[String],
    provider: &str,
) -> bool {
    if !is_inbox_provider(provider) {
        return false;
    }
    if !allowlist.is_empty() {
        let provider = provider.trim().to_ascii_lowercase();
        return allowlist
            .iter()
            .map(|s| s.trim().to_ascii_lowercase())
            .any(|p| p == provider);
    }
    // No pin: token is workspace-scoped. HMAC (when the provider signs) still
    // binds the payload. Do not accept a non-inbox path.
    let _ = follow_connected;
    true
}

/// True when this inbox key is allowed by the monitor filters (empty = allow all).
pub fn item_matches_filters(filters: &Value, provider: &str, key: &str) -> bool {
    let key = key.trim();
    match provider {
        "github" => {
            let repo = key.split('#').next().unwrap_or(key);
            listed_hit(&github_repos(filters), repo)
        }
        "gitlab" => {
            let project = key.split('#').next().unwrap_or(key);
            listed_hit(&gitlab_projects(filters), project)
        }
        "gitea" => {
            let repo = key.split('#').next().unwrap_or(key);
            listed_hit(&gitea_repos(filters), repo)
        }
        "gogs" => {
            let repo = key.split('#').next().unwrap_or(key);
            listed_hit(&gogs_repos(filters), repo)
        }
        "slack" => {
            let channel = key.split(':').next().unwrap_or(key);
            listed_hit(&slack_channels(filters), channel)
        }
        "linear" => linear_team_hit(&linear_teams(filters), key),
        // Webhook keys are card/page ids. Board/database allowlists are applied
        // on the poll path before listing children.
        "trello" | "notion" => true,
        _ => true,
    }
}

/// Slack history/event rows that are not user work (joins, bots, topic changes).
pub fn slack_message_is_noise(subtype: Option<&str>, text: &str) -> bool {
    if slack_system_subtype(subtype) {
        return true;
    }
    slack_system_text(text)
}

fn slack_system_subtype(subtype: Option<&str>) -> bool {
    matches!(
        subtype.unwrap_or("").trim(),
        "channel_join"
            | "channel_leave"
            | "channel_topic"
            | "channel_purpose"
            | "channel_name"
            | "channel_archive"
            | "channel_unarchive"
            | "group_join"
            | "group_leave"
            | "group_topic"
            | "group_purpose"
            | "group_name"
            | "group_archive"
            | "group_unarchive"
            | "bot_message"
            | "message_changed"
            | "message_deleted"
            | "pinned_item"
            | "unpinned_item"
            | "ekm_access_denied"
            | "channel_posting_permissions"
            | "reminder_add"
    )
}

fn slack_system_text(text: &str) -> bool {
    let t = text.trim();
    if t.starts_with("Fleet run ") {
        return true;
    }
    let lower = t.to_ascii_lowercase();
    const TAILS: &[&str] = &[
        "has joined the channel",
        "has left the channel",
        "has joined the group",
        "has left the group",
        "set the channel topic",
        "set the channel purpose",
        "set the channel description",
        "renamed the channel",
    ];
    TAILS
        .iter()
        .any(|needle| lower == *needle || lower.ends_with(&format!(" {needle}")))
}

/// Channel + history/event row → inbox key/title/summary, or none if noise.
pub fn slack_inbox_item(channel: &str, message: &Value) -> Option<(String, String, String)> {
    if channel.trim().is_empty() {
        return None;
    }
    let ts = message.get("ts").and_then(|v| v.as_str()).unwrap_or("");
    if ts.is_empty() {
        return None;
    }
    let subtype = message.get("subtype").and_then(|v| v.as_str());
    let text = message
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim();
    if text.is_empty() || slack_message_is_noise(subtype, text) {
        return None;
    }
    Some((
        format!("{channel}:{ts}"),
        text.chars().take(80).collect(),
        text.chars().take(4000).collect(),
    ))
}

/// Per-channel Slack history: skip a bad channel; fail only if none succeeded.
pub fn slack_poll_outcome(
    ok_channels: usize,
    ingested: usize,
    channel_errors: &[String],
) -> Result<usize, String> {
    if ok_channels == 0 && !channel_errors.is_empty() {
        Err(channel_errors.join("; "))
    } else {
        Ok(ingested)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_connected_picks_up_new_apps() {
        let watching = resolve_watching(true, &[], &["github".into(), "slack".into()]);
        assert_eq!(watching, vec!["github", "slack"]);
        let later = resolve_watching(
            true,
            &["github".into()],
            &["github".into(), "jira".into(), "slack".into()],
        );
        assert_eq!(later, vec!["github", "jira", "slack"]);
    }

    #[test]
    fn pinned_allowlist_stays_pinned() {
        let watching = resolve_watching(
            false,
            &["github".into(), "linear".into()],
            &["github".into(), "jira".into(), "linear".into()],
        );
        assert_eq!(watching, vec!["github", "linear"]);
    }

    #[test]
    fn empty_allowlist_without_follow_watches_nothing() {
        let watching = resolve_watching(false, &[], &["trello".into(), "github".into()]);
        assert!(watching.is_empty());
    }

    #[test]
    fn drops_non_inbox_and_disconnected() {
        let watching = resolve_watching(
            true,
            &[],
            &["github".into(), "figma".into(), "discord".into()],
        );
        assert_eq!(watching, vec!["github"]);
    }

    #[test]
    fn merge_legacy_github_repos_land_in_filters() {
        let merged = merge_filters(
            &json!({}),
            None,
            Some(&["acme/api".into()]),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            github_repos(&merged),
            vec!["acme/api"],
            "github_repos must land as filters.github.repos"
        );
    }

    #[test]
    fn merge_legacy_jira_jql_lands_in_filters() {
        let merged = merge_filters(
            &json!({}),
            None,
            None,
            Some("project = ENG"),
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            jira_jql(&merged).as_deref(),
            Some("project = ENG"),
            "jira_jql must land as filters.jira.jql"
        );
    }

    #[test]
    fn merge_legacy_slack_channels_land_in_filters() {
        let merged = merge_filters(
            &json!({}),
            None,
            None,
            None,
            Some(&["#eng".into()]),
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            slack_channels(&merged),
            vec!["#eng"],
            "slack_channels must land as filters.slack.channels"
        );
    }

    #[test]
    fn merge_legacy_gitlab_projects_land_in_filters() {
        let merged = merge_filters(
            &json!({}),
            None,
            None,
            None,
            None,
            None,
            Some(&["acme/app".into()]),
            None,
            None,
        );
        assert_eq!(
            gitlab_projects(&merged),
            vec!["acme/app"],
            "gitlab_projects must land as filters.gitlab.projects"
        );
    }

    #[test]
    fn merge_legacy_linear_teams_land_in_filters() {
        let merged = merge_filters(
            &json!({}),
            None,
            None,
            None,
            None,
            None,
            None,
            Some(&["ENG".into()]),
            None,
        );
        assert_eq!(
            linear_teams(&merged),
            vec!["ENG"],
            "linear_teams must land as filters.linear.teams"
        );
    }

    #[test]
    fn merge_omitted_notion_stays_empty() {
        let merged = merge_filters(
            &json!({}),
            None,
            Some(&["acme/api".into()]),
            Some("project = ENG"),
            Some(&["#eng".into()]),
            None,
            Some(&["acme/app".into()]),
            Some(&["ENG".into()]),
            None,
        );
        assert!(
            notion_databases(&merged).is_empty(),
            "omitted notion_databases must stay empty when other legacy fields land"
        );
    }

    #[test]
    fn github_repo_allowlist_is_exact() {
        let merged = merge_filters(
            &json!({}),
            None,
            Some(&["acme/api".into()]),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert!(
            item_matches_filters(&merged, "github", "acme/api#9"),
            "acme/api#9 must match repo allowlist acme/api"
        );
        assert!(
            !item_matches_filters(&merged, "github", "other/repo#1"),
            "other/repo must not match acme/api"
        );
        assert!(
            !item_matches_filters(&merged, "github", "acme/api-extra#1"),
            "repo allowlist must be exact, not a prefix of owner/name"
        );
    }

    #[test]
    fn linear_team_eng_does_not_match_engineering() {
        let merged = merge_filters(
            &json!({}),
            None,
            None,
            None,
            None,
            None,
            None,
            Some(&["ENG".into()]),
            None,
        );
        assert!(
            item_matches_filters(&merged, "linear", "ENG-12"),
            "ENG-12 must match team ENG"
        );
        assert!(
            !item_matches_filters(&merged, "linear", "OPS-1"),
            "OPS-1 must not match team ENG"
        );
        assert!(
            !item_matches_filters(&merged, "linear", "ENGINEERING-1"),
            "linear team ENG must not match ENGINEERING-1"
        );
    }

    #[test]
    fn merge_filters_legacy_fields_win_over_existing_and_incoming() {
        let merged = merge_filters(
            &json!({ "github": { "repos": ["old/repo"] } }),
            Some(&json!({ "github": { "repos": ["old/repo"] } })),
            Some(&["acme/api".into()]),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            github_repos(&merged),
            vec!["acme/api"],
            "legacy github_repos must overwrite existing and incoming filters.github.repos"
        );
    }

    #[test]
    fn merge_filters_empty_jira_jql_removes_jira() {
        let merged = merge_filters(
            &json!({ "jira": { "jql": "project=ENG" } }),
            None,
            None,
            Some(""),
            None,
            None,
            None,
            None,
            None,
        );
        assert!(
            merged.get("jira").is_none(),
            "empty jira_jql must remove the jira key"
        );
    }

    #[test]
    fn merge_filters_github_repos_require_owner_slash_name() {
        let merged = merge_filters(
            &json!({}),
            None,
            Some(&["acme".into(), "acme/api".into()]),
            None,
            None,
            None,
            None,
            None,
            None,
        );
        assert_eq!(
            github_repos(&merged),
            vec!["acme/api"],
            "github_repos without owner/name slash must be dropped"
        );
    }

    #[test]
    fn follow_connected_webhook_still_requires_listed_provider() {
        assert!(
            !webhook_provider_allowed(true, &["slack".into()], "github"),
            "follow_connected must not ingest github when the pin list is slack-only"
        );
    }

    #[test]
    fn empty_allowlist_accepts_inbox_provider() {
        assert!(
            webhook_provider_allowed(false, &[], "jira"),
            "empty pin is workspace-scoped; inbox path providers are accepted"
        );
    }

    #[test]
    fn pinned_webhook_accepts_listed_inbox_provider() {
        assert!(
            webhook_provider_allowed(false, &["slack".into(), "linear".into()], "slack"),
            "pinned slack+linear must ingest slack"
        );
    }

    #[test]
    fn pinned_webhook_rejects_other_providers() {
        assert!(
            !webhook_provider_allowed(false, &["slack".into()], "github"),
            "pinned slack must not ingest github"
        );
    }

    #[test]
    fn webhook_follow_connected_rejects_unknown_provider() {
        assert!(
            !webhook_provider_allowed(true, &[], "figma"),
            "follow_connected must still reject non-inbox figma"
        );
    }

    #[test]
    fn webhook_allowlist_match_is_trimmed_and_case_insensitive() {
        assert!(
            webhook_provider_allowed(false, &[" Slack ".into()], "SLACK"),
            "pinned allowlist must match after trim and ascii lowercase on both sides"
        );
    }

    #[test]
    fn webhook_rejects_non_inbox_even_when_listed() {
        assert!(
            !webhook_provider_allowed(false, &["figma".into(), "slack".into()], "figma"),
            "is_inbox_provider must reject figma even when the pin list includes it"
        );
    }

    #[test]
    fn trello_board_allowlist_is_exact() {
        let filters = json!({ "trello": { "boards": ["abc"] } });
        let allow = trello_boards(&filters);
        assert!(
            listed_hit(&allow, "ABC"),
            "board allowlist abc must match ABC"
        );
        assert!(
            !listed_hit(&allow, "abcd"),
            "board allowlist abc must not prefix-match abcd"
        );
        assert!(
            !listed_hit(&allow, "abc-extra"),
            "board allowlist abc must not prefix-match abc-extra"
        );
    }

    #[test]
    fn item_matches_filters_trello_ignores_board_allowlist() {
        let filters = json!({ "trello": { "boards": ["abc"] } });
        assert!(
            item_matches_filters(&filters, "trello", "any-card-id"),
            "trello webhook keys are card ids; board allowlist must not filter them"
        );
    }

    #[test]
    fn merge_filters_legacy_trello_boards_overwrites() {
        let merged = merge_filters(
            &json!({}),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(&["board-1".into()]),
        );
        assert_eq!(
            trello_boards(&merged).first().map(String::as_str),
            Some("board-1"),
            "legacy trello_boards must land as filters.trello.boards[0]"
        );
    }

    #[test]
    fn slack_join_and_bot_are_noise() {
        assert!(slack_message_is_noise(
            Some("channel_join"),
            "<@U1> has joined the channel"
        ));
        assert!(slack_message_is_noise(
            None,
            "vinicius has joined the channel"
        ));
        assert!(slack_message_is_noise(
            Some("bot_message"),
            "hello from a bot"
        ));
        assert!(slack_message_is_noise(None, "Fleet run abc started"));
        assert!(!slack_message_is_noise(
            None,
            "please look at the weather-app ticket"
        ));
        assert!(
            !slack_message_is_noise(None, "has anyone joined the standup?"),
            "user text that only mentions joining must still ingest"
        );
    }

    #[test]
    fn slack_inbox_item_skips_join_keeps_user_text() {
        let join = json!({
            "ts": "1.1",
            "subtype": "channel_join",
            "text": "<@U1> has joined the channel"
        });
        assert!(slack_inbox_item("C1", &join).is_none());
        let user = json!({
            "ts": "2.2",
            "text": "please look at weather-app"
        });
        let (key, title, summary) = slack_inbox_item("C1", &user).expect("user message");
        assert_eq!(key, "C1:2.2");
        assert_eq!(title, "please look at weather-app");
        assert_eq!(summary, "please look at weather-app");
    }

    #[test]
    fn slack_poll_skips_one_bad_channel_when_another_ok() {
        assert_eq!(
            slack_poll_outcome(1, 5, &["Cbad: Slack API: not_in_channel".into()]).unwrap(),
            5
        );
        let err =
            slack_poll_outcome(0, 0, &["Cbad: Slack API: not_in_channel".into()]).unwrap_err();
        assert!(err.contains("not_in_channel"), "{err}");
        assert_eq!(slack_poll_outcome(1, 0, &[]).unwrap(), 0);
    }
}
