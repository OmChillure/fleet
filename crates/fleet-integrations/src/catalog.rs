//! Platforms users link via OAuth so the fleet / AI can use them.

use crate::models::{AuthMethod, IntegrationAvailability, IntegrationCategory, IntegrationView};

/// Static catalog (connection + oauth_configured filled in routes).
/// Inbox and Integrations share these ids. The database seed matches this map.
pub fn shared_category(provider: &str) -> (&'static str, &'static str) {
    match provider {
        "github" | "gitlab" | "gitea" | "gogs" | "bitbucket" => ("source_control", "Source control"),
        "jira" | "linear" | "asana" | "trello" => ("issue_trackers", "Issue trackers"),
        "slack" | "teams" | "discord" => ("chat", "Chat"),
        "notion" | "confluence" | "google_drive" => ("docs", "Docs"),
        "figma" => ("design", "Design"),
        _ => ("", ""),
    }
}

pub fn catalog() -> Vec<IntegrationView> {
    let mut items = raw_catalog();
    for item in &mut items {
        let (id, label) = shared_category(&item.id);
        if !id.is_empty() {
            item.category_id = id.to_string();
            item.category_label = label.to_string();
        }
    }
    items
}

fn raw_catalog() -> Vec<IntegrationView> {
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
            connections: Vec::new(),
            category_id: String::new(),
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
            connections: Vec::new(),
            category_id: String::new(),
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
            connections: Vec::new(),
            category_id: String::new(),
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
            connections: Vec::new(),
            category_id: String::new(),
        },
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
            connections: Vec::new(),
            category_id: String::new(),
        },
        IntegrationView {
            id: "gitea".into(),
            name: "Gitea".into(),
            description: "Sign in to a Gitea instance so agents can work with issues, pull requests, and commits.".into(),
            category: IntegrationCategory::CodeVersioning,
            category_label: "Source control".into(),
            availability: IntegrationAvailability::Available,
            icon: "gitea".into(),
            docs_url: Some("https://docs.gitea.com/development/oauth2-provider".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            oauth_callback_url: None,
            oauth_app_create_url: None,
            connected: false,
            connection: None,
            connections: Vec::new(),
            category_id: String::new(),
        },
        IntegrationView {
            id: "gogs".into(),
            name: "Gogs".into(),
            description: "Sign in to a Gogs instance so agents can work with issues, files, and commits.".into(),
            category: IntegrationCategory::CodeVersioning,
            category_label: "Source control".into(),
            availability: IntegrationAvailability::Available,
            icon: "gogs".into(),
            docs_url: Some("https://gogs.io/docs/features/authentication".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            oauth_callback_url: None,
            oauth_app_create_url: None,
            connected: false,
            connection: None,
            connections: Vec::new(),
            category_id: String::new(),
        },
        IntegrationView {
            id: "bitbucket".into(),
            name: "Bitbucket".into(),
            description: "Connect Bitbucket Cloud for PRs and pipelines.".into(),
            category: IntegrationCategory::CodeVersioning,
            category_label: "Source control".into(),
            availability: IntegrationAvailability::Available,
            icon: "bitbucket".into(),
            docs_url: Some("https://developer.atlassian.com/cloud/bitbucket/oauth-2/".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            oauth_callback_url: None,
            oauth_app_create_url: None,
            connected: false,
            connection: None,
            connections: Vec::new(),
            category_id: String::new(),
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
            connections: Vec::new(),
            category_id: String::new(),
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
            connections: Vec::new(),
            category_id: String::new(),
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
            connections: Vec::new(),
            category_id: String::new(),
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
            connections: Vec::new(),
            category_id: String::new(),
        },
        IntegrationView {
            id: "confluence".into(),
            name: "Confluence".into(),
            description: "Pull Confluence pages as agent context and publish status.".into(),
            category: IntegrationCategory::TaskControl,
            category_label: "Docs".into(),
            availability: IntegrationAvailability::Available,
            icon: "confluence".into(),
            docs_url: Some("https://developer.atlassian.com/cloud/confluence/oauth-2-3lo-apps/".into()),
            auth_method: AuthMethod::Oauth,
            oauth_configured: false,
            oauth_start_path: None,
            oauth_callback_url: None,
            oauth_app_create_url: None,
            connected: false,
            connection: None,
            connections: Vec::new(),
            category_id: String::new(),
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
            connections: Vec::new(),
            category_id: String::new(),
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
            connections: Vec::new(),
            category_id: String::new(),
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
            connections: Vec::new(),
            category_id: String::new(),
        },
    ]
}

#[cfg(test)]
mod shared_category_tests {
    use super::*;

    #[test]
    fn every_catalog_provider_has_the_shared_category() {
        for item in catalog() {
            let (id, label) = shared_category(&item.id);
            assert_eq!(item.category_id, id, "{}", item.id);
            assert_eq!(item.category_label, label, "{}", item.id);
            assert!(!id.is_empty(), "{}", item.id);
        }
    }
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
        "gitea" => "http://localhost:3033/user/settings/applications",
        "gogs" => "http://localhost:3034/user/login",
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
        "bitbucket" => "https://support.atlassian.com/bitbucket-cloud/docs/use-oauth-on-bitbucket-cloud/",
        "confluence" => "https://developer.atlassian.com/console/myapps/",
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
        let mut seen = HashSet::new();
        for view in &list {
            assert!(
                seen.insert(view.id.as_str()),
                "duplicate catalog id {}; find_provider first-match would hide later rows",
                view.id
            );
        }
    }

    #[test]
    fn catalog_lists_all_published_provider_ids() {
        let list = catalog();
        let got: HashSet<&str> = list.iter().map(|i| i.id.as_str()).collect();
        let published: HashSet<&str> = HashSet::from([
            "github",
            "jira",
            "slack",
            "linear",
            "gitlab",
            "gitea",
            "gogs",
            "bitbucket",
            "teams",
            "discord",
            "figma",
            "notion",
            "confluence",
            "asana",
            "trello",
            "google_drive",
        ]);
        assert_eq!(
            got, published,
            "catalog ids must be the published set (missing {:?}, extra {:?})",
            published.difference(&got).copied().collect::<Vec<_>>(),
            got.difference(&published).copied().collect::<Vec<_>>(),
        );
    }

    #[test]
    fn catalog_ids_are_lowercase_url_slugs() {
        for view in catalog() {
            let id = view.id.as_str();
            assert_eq!(
                id,
                id.to_ascii_lowercase(),
                "catalog id {id:?} must be lowercase; put_oauth_app/start trim+lowercase then find_provider"
            );
            assert!(
                !id.is_empty()
                    && id
                        .chars()
                        .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_')),
                "catalog id {id:?} must match [a-z0-9_]+"
            );
        }
    }

    #[test]
    fn find_provider_unknown_is_none() {
        assert!(
            find_provider("notaprovider").is_none(),
            "find_provider(\"notaprovider\") must be None"
        );
        assert!(
            find_provider("").is_none(),
            "find_provider(\"\") must be None"
        );
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
    fn confluence_and_bitbucket_are_available_and_expose_oauth_start() {
        for id in ["confluence", "bitbucket"] {
            let c = find_provider(id).expect(id);
            assert_eq!(c.availability, IntegrationAvailability::Available, "{id}");
            let ws = "11111111-1111-1111-1111-111111111111";
            assert_eq!(
                oauth_start_path(ws, id).as_deref(),
                Some(
                    format!(
                        "/api/workspaces/11111111-1111-1111-1111-111111111111/integrations/{id}/oauth/start"
                    )
                    .as_str()
                ),
                "{id} must expose GET …/oauth/start"
            );
        }
        assert!(oauth_app_create_url("confluence").is_some());
        assert!(oauth_app_create_url("bitbucket").is_some());
    }
}
