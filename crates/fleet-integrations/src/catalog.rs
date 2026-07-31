//! Platforms users link via OAuth so the fleet / AI can use them.

use crate::models::{
    AuthMethod, IntegrationAvailability, IntegrationCategory, IntegrationView,
};

/// Static catalog (connection + oauth_configured filled in routes).
pub fn catalog() -> Vec<IntegrationView> {
    vec![
        IntegrationView {
            id: "github".into(),
            name: "GitHub".into(),
            description: "Sign in with GitHub so agents can work with issues, pull requests, and CI on your repos.".into(),
            category: IntegrationCategory::CodeVersioning,
            category_label: "Source control".into(),
            availability: IntegrationAvailability::Available,
            icon: "github".into(),
            docs_url: Some("https://docs.github.com/en/apps/oauth-apps".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            connected: false,
            connection: None,
        },
        IntegrationView {
            id: "jira".into(),
            name: "Jira".into(),
            description: "Authorize Atlassian so agents can pull tickets and update status as they work.".into(),
            category: IntegrationCategory::TaskControl,
            category_label: "Tasks".into(),
            availability: IntegrationAvailability::Available,
            icon: "jira".into(),
            docs_url: Some("https://developer.atlassian.com/cloud/jira/platform/oauth-2-3lo-apps/".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            connected: false,
            connection: None,
        },
        IntegrationView {
            id: "slack".into(),
            name: "Slack".into(),
            description: "Add Fleet to Slack for human-in-the-loop approvals and session updates.".into(),
            category: IntegrationCategory::Hitl,
            category_label: "HITL & chat".into(),
            availability: IntegrationAvailability::Available,
            icon: "slack".into(),
            docs_url: Some("https://api.slack.com/authentication/oauth-v2".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            connected: false,
            connection: None,
        },
        IntegrationView {
            id: "linear".into(),
            name: "Linear".into(),
            description: "Connect Linear so agents can pick up issues and keep cycle progress in sync.".into(),
            category: IntegrationCategory::TaskControl,
            category_label: "Tasks".into(),
            availability: IntegrationAvailability::Available,
            icon: "linear".into(),
            docs_url: Some("https://developers.linear.app/docs/oauth/authentication".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            connected: false,
            connection: None,
        },
        IntegrationView {
            id: "gitlab".into(),
            name: "GitLab".into(),
            description: "OAuth connect for merge requests and pipelines — coming soon.".into(),
            category: IntegrationCategory::CodeVersioning,
            category_label: "Source control".into(),
            availability: IntegrationAvailability::ComingSoon,
            icon: "gitlab".into(),
            docs_url: Some("https://docs.gitlab.com/ee/api/oauth2.html".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            connected: false,
            connection: None,
        },
        IntegrationView {
            id: "teams".into(),
            name: "Microsoft Teams".into(),
            description: "OAuth connect for approvals in Microsoft 365 — coming soon.".into(),
            category: IntegrationCategory::Notifications,
            category_label: "Notifications".into(),
            availability: IntegrationAvailability::ComingSoon,
            icon: "teams".into(),
            docs_url: Some("https://learn.microsoft.com/en-us/microsoftteams/platform/".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            connected: false,
            connection: None,
        },
    ]
}

pub fn find_provider(id: &str) -> Option<IntegrationView> {
    catalog().into_iter().find(|i| i.id == id)
}

pub fn is_known_provider(id: &str) -> bool {
    catalog().iter().any(|i| i.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn catalog_has_unique_ids() {
        let list = catalog();
        let ids: HashSet<_> = list.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids.len(), list.len());
        assert!(ids.contains("github"));
        assert!(ids.contains("slack"));
    }

    #[test]
    fn oauth_only() {
        for i in catalog() {
            assert_eq!(i.auth_method, AuthMethod::Oauth);
        }
    }
}
