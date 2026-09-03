//! Platforms users link via OAuth so the fleet / AI can use them.

use crate::models::{AuthMethod, IntegrationAvailability, IntegrationCategory, IntegrationView};

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
            oauth_callback_url: None,
            oauth_app_create_url: None,
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
            oauth_callback_url: None,
            oauth_app_create_url: None,
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
            oauth_callback_url: None,
            oauth_app_create_url: None,
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
            oauth_callback_url: None,
            oauth_app_create_url: None,
            connected: false,
            connection: None,
        },
        // --- Coming soon ---
        IntegrationView {
            id: "gitlab".into(),
            name: "GitLab".into(),
            description: "Sign in with GitLab so agents can work with issues, merge requests, and commits on your projects.".into(),
            category: IntegrationCategory::CodeVersioning,
            category_label: "Source control".into(),
            availability: IntegrationAvailability::Available,
            icon: "gitlab".into(),
            docs_url: Some("https://docs.gitlab.com/ee/api/oauth2.html".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            oauth_callback_url: None,
            oauth_app_create_url: None,
            connected: false,
            connection: None,
        },
        IntegrationView {
            id: "bitbucket".into(),
            name: "Bitbucket".into(),
            description: "Connect Bitbucket Cloud for PRs and pipelines. Tools are ready; OAuth connect is coming soon.".into(),
            category: IntegrationCategory::CodeVersioning,
            category_label: "Source control".into(),
            availability: IntegrationAvailability::ComingSoon,
            icon: "bitbucket".into(),
            docs_url: Some("https://developer.atlassian.com/cloud/bitbucket/oauth-2/".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            oauth_callback_url: None,
            oauth_app_create_url: None,
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
            oauth_callback_url: None,
            oauth_app_create_url: None,
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
            oauth_callback_url: None,
            oauth_app_create_url: None,
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
            oauth_callback_url: None,
            oauth_app_create_url: None,
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
            oauth_callback_url: None,
            oauth_app_create_url: None,
            connected: false,
            connection: None,
        },
        IntegrationView {
            id: "confluence".into(),
            name: "Confluence".into(),
            description: "Pull Confluence pages as agent context and publish status. Tools are ready; OAuth connect is coming soon.".into(),
            category: IntegrationCategory::TaskControl,
            category_label: "Docs".into(),
            availability: IntegrationAvailability::ComingSoon,
            icon: "confluence".into(),
            docs_url: Some("https://developer.atlassian.com/cloud/confluence/oauth-2-3lo-apps/".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            oauth_callback_url: None,
            oauth_app_create_url: None,
            connected: false,
            connection: None,
        },
        IntegrationView {
            id: "asana".into(),
            name: "Asana".into(),
            description: "Connect Asana so agents can pick up tasks and keep project progress in sync.".into(),
            category: IntegrationCategory::TaskControl,
            category_label: "Tasks".into(),
            availability: IntegrationAvailability::Available,
            icon: "asana".into(),
            docs_url: Some("https://developers.asana.com/docs/oauth".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            oauth_callback_url: None,
            oauth_app_create_url: None,
            connected: false,
            connection: None,
        },
        IntegrationView {
            id: "trello".into(),
            name: "Trello".into(),
            description: "Connect Trello so agents can read boards and move cards as they work.".into(),
            category: IntegrationCategory::TaskControl,
            category_label: "Tasks".into(),
            availability: IntegrationAvailability::Available,
            icon: "trello".into(),
            docs_url: Some("https://developer.atlassian.com/cloud/trello/guides/rest-api/authorization/".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            oauth_callback_url: None,
            oauth_app_create_url: None,
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
            oauth_callback_url: None,
            oauth_app_create_url: None,
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

/// Browser Connect path for **available** providers only.
///
/// Coming-soon rows (Bitbucket, Confluence, …) stay `None` even though
/// proxy tools may already exist. `enrich_views` must use this so a
/// tools-ready catalog cannot look like Connect is live.
pub fn oauth_start_path(workspace_id: impl std::fmt::Display, provider: &str) -> Option<String> {
    let view = find_provider(provider)?;
    if !matches!(view.availability, IntegrationAvailability::Available) {
        return None;
    }
    Some(format!(
        "/api/workspaces/{workspace_id}/integrations/{}/oauth/start",
        view.id
    ))
}

/// Console URL to register an OAuth application for this provider.
pub fn oauth_app_create_url(provider: &str) -> Option<&'static str> {
    Some(match provider {
        "github" => "https://github.com/settings/applications/new",
        "gitlab" => "https://gitlab.com/-/user_settings/applications",
        "jira" => "https://developer.atlassian.com/console/myapps/",
        "slack" => "https://api.slack.com/apps?new_app=1",
        "linear" => "https://linear.app/settings/api",
        "figma" => "https://www.figma.com/developers/apps",
        "notion" => "https://www.notion.so/my-integrations",
        "google_drive" => "https://console.cloud.google.com/apis/credentials",
        "teams" => {
            "https://portal.azure.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade"
        }
        "discord" => "https://discord.com/developers/applications",
        "trello" => "https://trello.com/power-ups/admin",
        "asana" => "https://app.asana.com/0/my-apps",
        _ => return None,
    })
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
        assert!(ids.contains("gitlab"));
        assert!(ids.contains("slack"));
        assert!(ids.contains("figma"));
        assert!(ids.contains("teams"));
        assert!(ids.contains("notion"));
        assert!(ids.contains("bitbucket"));
        assert!(ids.contains("confluence"));
    }

    #[test]
    fn oauth_only() {
        for i in catalog() {
            assert_eq!(i.auth_method, AuthMethod::Oauth);
        }
    }

    #[test]
    fn gitlab_is_available() {
        let g = find_provider("gitlab").expect("gitlab");
        assert_eq!(g.availability, IntegrationAvailability::Available);
        assert_eq!(
            oauth_app_create_url("gitlab"),
            Some("https://gitlab.com/-/user_settings/applications")
        );
    }

    #[test]
    fn trello_is_available() {
        let t = find_provider("trello").expect("trello");
        assert_eq!(t.availability, IntegrationAvailability::Available);
        assert_eq!(
            oauth_app_create_url("trello"),
            Some("https://trello.com/power-ups/admin")
        );
    }

    #[test]
    fn asana_is_available() {
        let a = find_provider("asana").expect("asana");
        assert_eq!(a.availability, IntegrationAvailability::Available);
        assert_eq!(
            oauth_app_create_url("asana"),
            Some("https://app.asana.com/0/my-apps")
        );
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

    /// Same contract http_api asserts for bitbucket: coming_soon, no oauth/start.
    /// Tools may be ready; that must not look like Connect is live.
    /// Static catalog `oauth_start_path` is None for every row (including
    /// GitHub) — the real Connect URL is filled by `oauth_start_path()`.
    #[test]
    fn confluence_is_coming_soon_and_must_not_expose_oauth_start() {
        assert!(is_known_provider("confluence"));
        let c = find_provider("confluence").expect("confluence");
        assert_eq!(c.id, "confluence");
        assert_eq!(c.availability, IntegrationAvailability::ComingSoon);
        assert_eq!(c.oauth_start_path, None);
        assert_eq!(c.oauth_app_create_url, None);
        assert_eq!(oauth_app_create_url("confluence"), None);
        assert!(!c.oauth_configured);
        assert!(!c.connected);
        assert!(c.connection.is_none());

        let ws = "11111111-1111-1111-1111-111111111111";
        assert_eq!(
            oauth_start_path(ws, "confluence"),
            None,
            "coming-soon Confluence must not expose GET …/oauth/start"
        );
        assert_eq!(
            oauth_start_path(ws, "github").as_deref(),
            Some(
                "/api/workspaces/11111111-1111-1111-1111-111111111111/integrations/github/oauth/start"
            ),
            "available providers still expose oauth/start so this test is not a tautology"
        );

        // Bitbucket http_api: coming_soon + null start path. Confluence matches.
        let b = find_provider("bitbucket").expect("bitbucket");
        assert_eq!(b.availability, IntegrationAvailability::ComingSoon);
        assert_eq!(oauth_start_path(ws, "bitbucket"), None);

        for i in catalog() {
            if i.availability == IntegrationAvailability::ComingSoon {
                assert_eq!(
                    oauth_start_path(ws, &i.id),
                    None,
                    "coming-soon providers must not expose oauth/start: {}",
                    i.id
                );
                assert_eq!(
                    oauth_app_create_url(&i.id),
                    None,
                    "coming-soon providers must not advertise an OAuth app console: {}",
                    i.id
                );
            } else {
                assert!(
                    oauth_start_path(ws, &i.id).is_some(),
                    "available provider must expose oauth/start: {}",
                    i.id
                );
            }
        }

        assert!(
            c.description.contains("Tools are ready"),
            "confluence tools exist: {}",
            c.description
        );
        let desc = c.description.to_ascii_lowercase();
        assert!(
            desc.contains("coming soon"),
            "confluence oauth still coming soon: {}",
            c.description
        );
        assert!(
            desc.contains("oauth") && desc.contains("coming soon"),
            "tools-ready copy must not read as Connect live: {}",
            c.description
        );
    }

    /// Proxy ops exist (`create_pr`, …) but Connect must stay off.
    /// Tools-ready copy must not look like OAuth start is live.
    ///
    /// Static catalog `oauth_start_path` is None for every row (including
    /// GitHub) — `enrich_views` fills Connect from `oauth_start_path()`.
    /// This test must call that function; a field-only assert is a tautology.
    #[test]
    fn bitbucket_is_coming_soon_and_must_not_expose_oauth_start() {
        assert!(is_known_provider("bitbucket"));
        let b = find_provider("bitbucket").expect("bitbucket");
        assert_eq!(b.id, "bitbucket");
        assert_eq!(b.availability, IntegrationAvailability::ComingSoon);
        assert_eq!(b.oauth_start_path, None);
        assert_eq!(b.oauth_app_create_url, None);
        assert_eq!(oauth_app_create_url("bitbucket"), None);
        assert!(!b.oauth_configured);
        assert!(!b.connected);
        assert!(b.connection.is_none());

        let ws = "11111111-1111-1111-1111-111111111111";
        assert_eq!(
            oauth_start_path(ws, "bitbucket"),
            None,
            "coming-soon Bitbucket must not expose GET …/oauth/start"
        );
        assert_eq!(
            oauth_start_path(ws, "github").as_deref(),
            Some(
                "/api/workspaces/11111111-1111-1111-1111-111111111111/integrations/github/oauth/start"
            ),
            "available providers still expose oauth/start so this test is not a tautology"
        );

        assert!(
            b.description.contains("Tools are ready"),
            "bitbucket tools exist: {}",
            b.description
        );
        let desc = b.description.to_ascii_lowercase();
        assert!(
            desc.contains("coming soon"),
            "bitbucket oauth still coming soon: {}",
            b.description
        );
        assert!(
            desc.contains("oauth") && desc.contains("coming soon"),
            "tools-ready copy must not read as Connect live: {}",
            b.description
        );
    }
}
