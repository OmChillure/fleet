use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

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

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct WorkspaceMember {
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub role: String,
    pub created_at: DateTime<Utc>,
}
