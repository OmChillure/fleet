use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow, Serialize)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub email_normalized: String,
    pub display_name: Option<String>,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub status: String,
    /// Cross-workspace operator. Not the workspace membership role `admin`.
    #[serde(default)]
    pub instance_admin: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Optimistic concurrency token for profile updates.
    pub version: i64,
}

impl User {
    pub fn is_active(&self) -> bool {
        self.status == "active"
    }
}

/// Safe subset for API responses (no password hash).
#[derive(Debug, Clone, Serialize)]
pub struct UserPublic {
    pub id: Uuid,
    pub email: String,
    pub display_name: Option<String>,
    #[serde(default)]
    pub instance_admin: bool,
    #[serde(default)]
    pub status: String,
}

impl From<User> for UserPublic {
    fn from(u: User) -> Self {
        Self {
            id: u.id,
            email: u.email,
            display_name: u.display_name,
            instance_admin: u.instance_admin,
            status: u.status,
        }
    }
}

impl From<&User> for UserPublic {
    fn from(u: &User) -> Self {
        Self {
            id: u.id,
            email: u.email.clone(),
            display_name: u.display_name.clone(),
            instance_admin: u.instance_admin,
            status: u.status.clone(),
        }
    }
}
