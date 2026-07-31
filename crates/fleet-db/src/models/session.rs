use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Auth session / refresh-token row (`auth_sessions`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AuthSession {
    pub id: Uuid,
    pub user_id: Uuid,
    pub refresh_token_hash: Vec<u8>,
    pub family_id: Uuid,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub absolute_expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub last_seen_at: DateTime<Utc>,
}

impl AuthSession {
    pub fn is_active(&self, now: DateTime<Utc>) -> bool {
        self.revoked_at.is_none()
            && self.expires_at > now
            && self.absolute_expires_at > now
    }
}

/// Parameters to insert a new auth session (login or refresh rotation).
#[derive(Debug, Clone)]
pub struct NewAuthSession {
    pub id: Uuid,
    pub user_id: Uuid,
    pub refresh_token_hash: Vec<u8>,
    pub family_id: Uuid,
    pub user_agent: Option<String>,
    /// Pre-validated IP string (or None) — stored as Postgres `inet`.
    pub ip: Option<String>,
    pub expires_at: DateTime<Utc>,
    pub absolute_expires_at: DateTime<Utc>,
}
