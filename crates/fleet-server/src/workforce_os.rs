//! Pure rules for the workforce OS.
//!
//! Work arrives as an issue. A crew may commit and open a pull request.
//! Merge, a customer-facing message, and a deploy wait for a person.
//! These functions do not touch the database.

use serde_json::{Map, Value};

/// Policy kind stored on `policy_approvals.kind`, or nothing when the
/// operation is ordinary crew work.
pub fn gate_kind(provider: &str, operation: &str) -> Option<&'static str> {
    let provider = provider.trim().to_ascii_lowercase();
    let operation = operation.trim().to_ascii_lowercase();
    match (provider.as_str(), operation.as_str()) {
        ("github" | "gitlab", "merge_pr") => Some("merge"),
        ("slack", "post_message") => Some("external_comms"),
        ("teams", "send_chat_message" | "send_channel_message") => Some("external_comms"),
        ("notion", "create_page" | "append_blocks") => Some("external_comms"),
        (_, "deploy") => Some("deploy"),
        _ => None,
    }
}

/// Stable identity for one GitHub issue inside a workspace.
pub fn github_source_ref(owner_repo: &str, number: u64) -> String {
    format!("github:{owner_repo}#{number}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntakeVerdict {
    Accept {
        source_ref: String,
        title: String,
        goal_md: String,
    },
    Refuse {
        source_ref: String,
        reason: &'static str,
    },
}

/// Decide whether an open GitHub issue is specific enough to become a goal.
/// Pull requests are filtered by the GitHub tool before they reach here.
pub fn analyze_github_issue(
    owner_repo: &str,
    number: u64,
    title: &str,
    body: &str,
    html_url: &str,
) -> IntakeVerdict {
    let source_ref = github_source_ref(owner_repo, number);
    let title = title.trim();
    let body = body.trim();
    if number == 0 || owner_repo.trim().is_empty() || !owner_repo.contains('/') {
        return IntakeVerdict::Refuse {
            source_ref,
            reason: "issue is missing a repository or number",
        };
    }
    if title.is_empty() {
        return IntakeVerdict::Refuse {
            source_ref,
            reason: "issue has no title",
        };
    }
    if title.chars().count() < 8 && body.chars().count() < 40 {
        return IntakeVerdict::Refuse {
            source_ref,
            reason: "issue is not specific enough to hand to a crew",
        };
    }
    let mut goal_md = format!("source: {source_ref}\n");
    if !html_url.is_empty() {
        goal_md.push_str(&format!("url: {html_url}\n"));
    }
    goal_md.push('\n');
    if body.is_empty() {
        goal_md.push_str(title);
    } else {
        goal_md.push_str(body);
    }
    IntakeVerdict::Accept {
        source_ref,
        title: title.chars().take(200).collect(),
        goal_md,
    }
}

/// Drop credential-shaped keys before an approval row is stored.
pub fn redact_args(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (k, v) in map {
                let key = k.to_ascii_lowercase();
                if matches!(
                    key.as_str(),
                    "token"
                        | "access_token"
                        | "refresh_token"
                        | "authorization"
                        | "password"
                        | "secret"
                        | "api_key"
                ) {
                    out.insert(k.clone(), Value::String("[redacted]".into()));
                } else {
                    out.insert(k.clone(), redact_args(v));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact_args).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_and_outbound_messages_wait_for_a_person() {
        assert_eq!(gate_kind("GitHub", "merge_pr"), Some("merge"));
        assert_eq!(gate_kind("gitlab", "merge_pr"), Some("merge"));
        assert_eq!(gate_kind("slack", "post_message"), Some("external_comms"));
        assert_eq!(
            gate_kind("teams", "send_channel_message"),
            Some("external_comms")
        );
        assert_eq!(gate_kind("notion", "create_page"), Some("external_comms"));
        assert_eq!(gate_kind("github", "deploy"), Some("deploy"));
    }

    #[test]
    fn crew_work_is_not_held() {
        assert_eq!(gate_kind("github", "create_pr"), None);
        assert_eq!(gate_kind("github", "commit_files"), None);
        assert_eq!(gate_kind("github", "list_issues"), None);
        assert_eq!(gate_kind("slack", "list_channels"), None);
        assert_eq!(gate_kind("notion", "search"), None);
    }

    #[test]
    fn vague_issues_are_refused_and_specific_ones_become_goals() {
        let vague = analyze_github_issue("acme/app", 4, "fix", "", "");
        assert_eq!(
            vague,
            IntakeVerdict::Refuse {
                source_ref: "github:acme/app#4".into(),
                reason: "issue is not specific enough to hand to a crew",
            }
        );
        let ok = analyze_github_issue(
            "acme/app",
            9,
            "Add a README",
            "Document how to run the tests.",
            "https://github.com/acme/app/issues/9",
        );
        match ok {
            IntakeVerdict::Accept {
                source_ref,
                title,
                goal_md,
            } => {
                assert_eq!(source_ref, "github:acme/app#9");
                assert_eq!(title, "Add a README");
                assert!(goal_md.contains("Document how to run the tests."));
                assert!(goal_md.contains("https://github.com/acme/app/issues/9"));
            }
            IntakeVerdict::Refuse { reason, .. } => panic!("accepted issue refused: {reason}"),
        }
    }

    #[test]
    fn redact_args_strips_tokens_and_keeps_the_pull_request() {
        let clean = redact_args(&json!({
            "owner": "acme",
            "repo": "app",
            "number": 9,
            "token": "ghs_secret",
        }));
        assert_eq!(clean["owner"], "acme");
        assert_eq!(clean["number"], 9);
        assert_eq!(clean["token"], "[redacted]");
    }
}
