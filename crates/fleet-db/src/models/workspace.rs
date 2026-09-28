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

/// Workspace network posture. Independent of where the job runs.
pub const EGRESS_INHERIT: &str = "inherit";
pub const EGRESS_NONE: &str = "none";
pub const EGRESS_ALLOWLIST: &str = "allowlist";
pub const EGRESS_OPEN: &str = "open";

/// `inherit` | `none` | `allowlist` | `open`.
pub fn parse_egress_mode(raw: &str) -> Option<&'static str> {
    match raw.trim() {
        EGRESS_INHERIT => Some(EGRESS_INHERIT),
        EGRESS_NONE => Some(EGRESS_NONE),
        EGRESS_ALLOWLIST => Some(EGRESS_ALLOWLIST),
        EGRESS_OPEN => Some(EGRESS_OPEN),
        _ => None,
    }
}

/// Allowlist entries split on commas, semicolons, and whitespace.
pub fn split_egress_allowlist(raw: &str) -> Vec<String> {
    raw.split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
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

    #[test]
    fn parse_egress_is_four_postures() {
        assert_eq!(parse_egress_mode(" inherit "), Some(EGRESS_INHERIT));
        assert_eq!(parse_egress_mode("none"), Some(EGRESS_NONE));
        assert_eq!(parse_egress_mode("allowlist"), Some(EGRESS_ALLOWLIST));
        assert_eq!(parse_egress_mode("open"), Some(EGRESS_OPEN));
        assert_eq!(parse_egress_mode("package_registries"), None);
        assert_eq!(parse_egress_mode("dns_only"), None);
        assert_eq!(
            split_egress_allowlist("10.0.0.0/8, github.com\n172.18.0.1/32"),
            vec![
                "10.0.0.0/8".to_string(),
                "github.com".to_string(),
                "172.18.0.1/32".to_string(),
            ]
        );
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
