use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

/// Canonical workspace_location wire strings stored on `workspaces.min_isolation`.
/// Where the repo lives — not agent_runtime (`run_mode` on the host).
/// Settings JSON still uses `min_isolation` (see fleet_routes.rs).
pub const MIN_ISOLATION_PROCESS: &str = "process";
pub const MIN_ISOLATION_CONTAINER: &str = "container";
pub const MIN_ISOLATION_VM: &str = "vm";

/// `process` | `container` | `vm` (workspace_location). Other values are rejected.
pub fn parse_min_isolation(raw: &str) -> Option<&'static str> {
    match raw.trim() {
        MIN_ISOLATION_PROCESS => Some(MIN_ISOLATION_PROCESS),
        MIN_ISOLATION_CONTAINER => Some(MIN_ISOLATION_CONTAINER),
        MIN_ISOLATION_VM => Some(MIN_ISOLATION_VM),
        _ => None,
    }
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct Workspace {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
}

/// Workspace plus the calling user's role (list memberships).
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct WorkspaceWithRole {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
    pub role: String,
}

impl WorkspaceWithRole {
    pub fn as_public(&self) -> WorkspacePublic {
        WorkspacePublic {
            id: self.id,
            slug: self.slug.clone(),
            name: self.name.clone(),
            role: self.role.clone(),
        }
    }
}

/// Safe subset for API responses.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspacePublic {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    pub role: String,
}

/// Workspace-level partners disclaimer ack.
///
/// Default is not accepted (`accepted_at` is `None`). Recording an ack
/// does not enable lending, borrowing, enqueue, or assignable runners.
#[derive(Debug, Clone, Serialize)]
pub struct PartnersDisclaimerAck {
    pub accepted: bool,
    pub accepted_at: Option<DateTime<Utc>>,
}

impl PartnersDisclaimerAck {
    pub fn from_accepted_at(accepted_at: Option<DateTime<Utc>>) -> Self {
        Self {
            accepted: accepted_at.is_some(),
            accepted_at,
        }
    }
}

impl From<WorkspaceWithRole> for WorkspacePublic {
    fn from(w: WorkspaceWithRole) -> Self {
        w.as_public()
    }
}

#[cfg(test)]
mod min_isolation_tests {
    use super::*;

    #[test]
    fn parse_accepts_canonical_strings_only() {
        assert_eq!(parse_min_isolation("process"), Some(MIN_ISOLATION_PROCESS));
        assert_eq!(
            parse_min_isolation(" container "),
            Some(MIN_ISOLATION_CONTAINER)
        );
        assert_eq!(parse_min_isolation("vm"), Some(MIN_ISOLATION_VM));
        assert_eq!(parse_min_isolation("native"), None);
        assert_eq!(parse_min_isolation("host"), None);
        assert_eq!(parse_min_isolation("k8s"), None);
        assert_eq!(parse_min_isolation("on_pc"), None);
        assert_eq!(parse_min_isolation("in_container"), None);
        assert_eq!(parse_min_isolation(""), None);
    }
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct WorkspaceMember {
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub role: String,
    pub created_at: DateTime<Utc>,
}

/// Member plus user identity for the members list.
#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct WorkspaceMemberPublic {
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub role: String,
    pub created_at: DateTime<Utc>,
    pub email: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct WorkspaceInvite {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub email_normalized: String,
    pub role: String,
    pub token: String,
    pub invited_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub accepted_user_id: Option<Uuid>,
    pub revoked_at: Option<DateTime<Utc>>,
}
