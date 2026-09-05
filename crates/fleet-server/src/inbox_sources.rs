//! Inbox adapters: which connected apps feed the watcher.

use serde_json::{Value, json};

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

/// Webhook ingest: follow-connected (or empty allowlist) accepts any inbox
/// provider. A pinned allowlist accepts only those providers.
pub fn webhook_provider_allowed(
    follow_connected: bool,
    allowlist: &[String],
    provider: &str,
) -> bool {
    if !is_inbox_provider(provider) {
        return false;
    }
    if follow_connected {
        return true;
    }
    if allowlist.is_empty() {
        return false;
    }
    let provider = provider.trim().to_ascii_lowercase();
    allowlist
        .iter()
        .map(|s| s.trim().to_ascii_lowercase())
        .any(|p| p == provider)
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
    fn merge_legacy_fields_into_filters() {
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
        assert_eq!(merged["gitlab"]["projects"][0], "acme/app");
        assert_eq!(merged["linear"]["teams"][0], "ENG");
        assert!(item_matches_filters(&merged, "github", "acme/api#9"));
        assert!(!item_matches_filters(&merged, "github", "other/repo#1"));
        assert!(item_matches_filters(&merged, "linear", "ENG-12"));
        assert!(!item_matches_filters(&merged, "linear", "OPS-1"));
        assert!(
            !item_matches_filters(&merged, "linear", "ENGINEERING-1"),
            "linear team ENG must not match ENGINEERING-1"
        );
        assert!(
            !item_matches_filters(&merged, "github", "acme/api-extra#1"),
            "repo allowlist must be exact, not a prefix of owner/name"
        );
        assert_eq!(merged["github"]["repos"][0], "acme/api");
        assert_eq!(merged["jira"]["jql"], "project = ENG");
        assert_eq!(slack_channels(&merged), vec!["#eng"]);
        assert!(notion_databases(&merged).is_empty());
    }

    #[test]
    fn pinned_webhook_rejects_other_providers() {
        assert!(webhook_provider_allowed(true, &["slack".into()], "github"));
        assert!(!webhook_provider_allowed(false, &[], "jira"));
        assert!(webhook_provider_allowed(
            false,
            &["slack".into(), "linear".into()],
            "slack"
        ));
        assert!(!webhook_provider_allowed(
            false,
            &["slack".into()],
            "github"
        ));
        assert!(!webhook_provider_allowed(true, &[], "figma"));
    }

    #[test]
    fn trello_board_allowlist_is_exact() {
        let filters = json!({ "trello": { "boards": ["abc"] } });
        assert!(listed_hit(&trello_boards(&filters), "abc"));
        assert!(listed_hit(&trello_boards(&filters), "ABC"));
        assert!(!listed_hit(&trello_boards(&filters), "other"));
        assert!(listed_hit(&[], "anything"));
    }
}
