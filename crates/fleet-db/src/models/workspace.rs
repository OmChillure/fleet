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

#[cfg(test)]
mod partners_disclaimer_tests {
    use super::*;

    #[test]
    fn missing_timestamp_is_not_accepted() {
        let ack = PartnersDisclaimerAck::from_accepted_at(None);
        assert!(!ack.accepted, "default (NULL) is not accepted");
        assert_eq!(ack.accepted_at, None);
    }

    #[test]
    fn recorded_timestamp_is_accepted() {
        let at = DateTime::parse_from_rfc3339("2026-08-20T12:00:00Z")
            .expect("rfc3339")
            .with_timezone(&Utc);
        let ack = PartnersDisclaimerAck::from_accepted_at(Some(at));
        assert!(ack.accepted);
        assert_eq!(ack.accepted_at, Some(at));
    }

    #[test]
    fn ack_serializes_timestamp_only() {
        let at = DateTime::parse_from_rfc3339("2026-08-20T12:00:00Z")
            .expect("rfc3339")
            .with_timezone(&Utc);
        let json = serde_json::to_value(PartnersDisclaimerAck::from_accepted_at(Some(at)))
            .expect("serialize");
        let obj = json.as_object().expect("object");
        assert_eq!(obj.get("accepted"), Some(&serde_json::json!(true)));
        assert_eq!(
            obj.get("accepted_at"),
            Some(&serde_json::json!("2026-08-20T12:00:00Z"))
        );
        // Ack is a recorded timestamp only — no lend/borrow/enqueue flags.
        assert_eq!(obj.len(), 2);
        assert!(!obj.contains_key("lending"));
        assert!(!obj.contains_key("borrowing"));
        assert!(!obj.contains_key("enqueue"));
        assert!(!obj.contains_key("assignable"));
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
