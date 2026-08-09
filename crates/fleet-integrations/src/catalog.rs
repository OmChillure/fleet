//! Platforms users link via OAuth so the fleet / AI can use them.

use crate::models::{
    AuthMethod, IntegrationAvailability, IntegrationCategory, IntegrationView,
};

/// Static catalog (connection + oauth_configured filled in routes).
pub fn catalog() -> Vec<IntegrationView> {
    vec![
        // --- Available (OAuth implemented) ---
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
            description: "Add Fleet to Slack for human in the loop approvals and session updates.".into(),
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
        // --- Coming soon ---
        IntegrationView {
            id: "gitlab".into(),
            name: "GitLab".into(),
            description: "OAuth connect for merge requests and pipelines. Coming soon.".into(),
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
            id: "bitbucket".into(),
            name: "Bitbucket".into(),
            description: "Connect Bitbucket Cloud for PRs and pipelines. Coming soon.".into(),
            category: IntegrationCategory::CodeVersioning,
            category_label: "Source control".into(),
            availability: IntegrationAvailability::ComingSoon,
            icon: "bitbucket".into(),
            docs_url: Some("https://developer.atlassian.com/cloud/bitbucket/oauth-2/".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            connected: false,
            connection: None,
        },
        IntegrationView {
            id: "teams".into(),
            name: "Microsoft Teams".into(),
            description: "Connect Microsoft 365 so agents can post approvals and session updates in Teams.".into(),
            category: IntegrationCategory::Notifications,
            category_label: "Notifications".into(),
            availability: IntegrationAvailability::Available,
            icon: "teams".into(),
            docs_url: Some("https://learn.microsoft.com/en-us/microsoftteams/platform/".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            connected: false,
            connection: None,
        },
        IntegrationView {
            id: "discord".into(),
            name: "Discord".into(),
            description: "Connect Discord for HITL approvals and alerts in your servers.".into(),
            category: IntegrationCategory::Hitl,
            category_label: "HITL & chat".into(),
            availability: IntegrationAvailability::Available,
            icon: "discord".into(),
            docs_url: Some("https://discord.com/developers/docs/topics/oauth2".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            connected: false,
            connection: None,
        },
        IntegrationView {
            id: "figma".into(),
            name: "Figma".into(),
            description: "Authorize Figma so agents can read design files and comments while implementing UI.".into(),
            category: IntegrationCategory::TaskControl,
            category_label: "Design".into(),
            availability: IntegrationAvailability::Available,
            icon: "figma".into(),
            docs_url: Some("https://www.figma.com/developers/api#oauth2".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            connected: false,
            connection: None,
        },
        IntegrationView {
            id: "notion".into(),
            name: "Notion".into(),
            description: "Connect Notion so agents can read specs and wiki pages as context.".into(),
            category: IntegrationCategory::TaskControl,
            category_label: "Docs".into(),
            availability: IntegrationAvailability::Available,
            icon: "notion".into(),
            docs_url: Some("https://developers.notion.com/docs/authorization".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            connected: false,
            connection: None,
        },
        IntegrationView {
            id: "confluence".into(),
            name: "Confluence".into(),
            description: "Pull Confluence pages as agent context and publish status. Coming soon.".into(),
            category: IntegrationCategory::TaskControl,
            category_label: "Docs".into(),
            availability: IntegrationAvailability::ComingSoon,
            icon: "confluence".into(),
            docs_url: Some("https://developer.atlassian.com/cloud/confluence/oauth-2-3lo-apps/".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            connected: false,
            connection: None,
        },
        IntegrationView {
            id: "asana".into(),
            name: "Asana".into(),
            description: "Connect Asana projects so agents can pick up tasks. Coming soon.".into(),
            category: IntegrationCategory::TaskControl,
            category_label: "Tasks".into(),
            availability: IntegrationAvailability::ComingSoon,
            icon: "asana".into(),
            docs_url: Some("https://developers.asana.com/docs/oauth".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            connected: false,
            connection: None,
        },
        IntegrationView {
            id: "trello".into(),
            name: "Trello".into(),
            description: "Boards and cards for lightweight task control. Coming soon.".into(),
            category: IntegrationCategory::TaskControl,
            category_label: "Tasks".into(),
            availability: IntegrationAvailability::ComingSoon,
            icon: "trello".into(),
            docs_url: Some("https://developer.atlassian.com/cloud/trello/guides/rest-api/authorization/".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            connected: false,
            connection: None,
        },
        IntegrationView {
            id: "google_drive".into(),
            name: "Google Drive".into(),
            description: "Connect Drive so agents can read docs and assets for context.".into(),
            category: IntegrationCategory::TaskControl,
            category_label: "Docs".into(),
            availability: IntegrationAvailability::Available,
            icon: "google_drive".into(),
            docs_url: Some("https://developers.google.com/identity/protocols/oauth2".into()),
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
        assert!(ids.contains("figma"));
        assert!(ids.contains("teams"));
        assert!(ids.contains("notion"));
    }

    #[test]
    fn oauth_only() {
        for i in catalog() {
            assert_eq!(i.auth_method, AuthMethod::Oauth);
        }
    }

    #[test]
    fn no_em_dash_in_copy() {
        for i in catalog() {
            assert!(
                !i.description.contains('—') && !i.description.contains('–'),
                "{} description still has a dash",
                i.id
            );
        }
    }
}
