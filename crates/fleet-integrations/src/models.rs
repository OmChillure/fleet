//! Public API types for OAuth integrations.

use fleet_db::IntegrationConnectionPublic;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationCategory {
    TaskControl,
    CodeVersioning,
    Hitl,
    Notifications,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationAvailability {
    Available,
    ComingSoon,
}

/// How the user links this platform (always OAuth for web).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    Oauth,
}

/// Catalog entry + live workspace connection + whether OAuth env is set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationView {
    pub id: String,
    pub name: String,
    pub description: String,
    pub category: IntegrationCategory,
    pub category_label: String,
    /// Shared id from `integration_categories` (`source_control`, `chat`, …).
    #[serde(default)]
    pub category_id: String,
    pub availability: IntegrationAvailability,
    pub icon: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docs_url: Option<String>,
    pub auth_method: AuthMethod,
    /// Env *or* workspace OAuth app credentials exist for this provider.
    pub oauth_configured: bool,
    /// Browser navigates here to start OAuth (available providers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth_start_path: Option<String>,
    /// Redirect URI to paste into the provider's OAuth app settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth_callback_url: Option<String>,
    /// Where to create an OAuth app for this provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth_app_create_url: Option<String>,
    pub connected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub connection: Option<IntegrationConnectionPublic>,
    /// Every live connection for this provider. `connection` is the newest.
    #[serde(default)]
    pub connections: Vec<IntegrationConnectionPublic>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationCategoryView {
    pub id: String,
    pub label: String,
    pub subtitle: String,
    pub sort_order: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationListResponse {
    pub workspace_id: String,
    #[serde(default)]
    pub categories: Vec<IntegrationCategoryView>,
    pub integrations: Vec<IntegrationView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DisconnectResponse {
    pub provider: String,
    pub disconnected: bool,
}
