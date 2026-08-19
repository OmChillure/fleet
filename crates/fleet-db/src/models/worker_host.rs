use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const WORKER_HOST_STATUS_PENDING: &str = "pending";
pub const WORKER_HOST_STATUS_ONLINE: &str = "online";
pub const WORKER_HOST_STATUS_DISABLED: &str = "disabled";
pub const WORKER_HOST_STATUS_ERROR: &str = "error";

/// Stored `online` older than this is serialized as `offline`.
pub const WORKER_HOST_OFFLINE_AFTER: Duration = Duration::seconds(30);

/// Durable paired worker machine (`worker_hosts`).
#[derive(Debug, Clone, sqlx::FromRow, Serialize, Deserialize)]
pub struct WorkerHost {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub status: String,
    pub enabled: bool,
    pub paused: bool,
    pub drain: bool,
    pub allow_host: bool,
    pub allow_container: bool,
    pub max_concurrent: i32,
    pub max_job_timeout_s: i32,
    pub labels: Vec<String>,
    #[serde(skip_serializing)]
    pub machine_token_hash: Option<String>,
    pub advertised_host: bool,
    pub advertised_container: bool,
    pub platform: Option<String>,
    pub hostname: Option<String>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub current_jobs: i32,
    pub advertised_max_concurrent: Option<i32>,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub paired_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
}

impl WorkerHost {
    /// Plane ∩ advertised. Worker-local `run_*` is applied on the box, not here.
    pub fn effective_host(&self) -> bool {
        self.allow_host && self.advertised_host
    }

    pub fn effective_container(&self) -> bool {
        self.allow_container && self.advertised_container
    }

    /// `online` with a stale `last_seen_at` is reported as `offline`.
    pub fn public_status(&self, now: DateTime<Utc>) -> &str {
        if self.status == WORKER_HOST_STATUS_ONLINE {
            match self.last_seen_at {
                Some(seen) if now.signed_duration_since(seen) <= WORKER_HOST_OFFLINE_AFTER => {
                    WORKER_HOST_STATUS_ONLINE
                }
                _ => "offline",
            }
        } else {
            &self.status
        }
    }

    pub fn as_public(&self, now: DateTime<Utc>) -> WorkerHostPublic {
        WorkerHostPublic::from_host(self, now)
    }
}

/// API/UI shape — never includes hashes or raw tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerHostPublic {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub status: String,
    pub enabled: bool,
    pub paused: bool,
    pub drain: bool,
    pub allow_host: bool,
    pub allow_container: bool,
    pub max_concurrent: i32,
    pub max_job_timeout_s: i32,
    pub labels: Vec<String>,
    pub advertised_host: bool,
    pub advertised_container: bool,
    pub platform: Option<String>,
    pub hostname: Option<String>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub paired_at: Option<DateTime<Utc>>,
}

impl WorkerHostPublic {
    pub fn from_host(host: &WorkerHost, now: DateTime<Utc>) -> Self {
        Self {
            id: host.id,
            workspace_id: host.workspace_id,
            name: host.name.clone(),
            status: host.public_status(now).to_string(),
            enabled: host.enabled,
            paused: host.paused,
            drain: host.drain,
            allow_host: host.allow_host,
            allow_container: host.allow_container,
            max_concurrent: host.max_concurrent,
            max_job_timeout_s: host.max_job_timeout_s,
            labels: host.labels.clone(),
            advertised_host: host.advertised_host,
            advertised_container: host.advertised_container,
            platform: host.platform.clone(),
            hostname: host.hostname.clone(),
            last_seen_at: host.last_seen_at,
            last_error: host.last_error.clone(),
            created_at: host.created_at,
            paired_at: host.paired_at,
        }
    }
}

/// Insert a pending host (pair token is a separate row).
#[derive(Debug, Clone)]
pub struct NewWorkerHost {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub created_by: Option<Uuid>,
    pub allow_host: bool,
    pub allow_container: bool,
    pub max_concurrent: i32,
    pub max_job_timeout_s: i32,
    pub labels: Vec<String>,
}

impl NewWorkerHost {
    pub fn new(
        id: Uuid,
        workspace_id: Uuid,
        name: impl Into<String>,
        created_by: Option<Uuid>,
    ) -> Self {
        Self {
            id,
            workspace_id,
            name: name.into(),
            created_by,
            allow_host: true,
            allow_container: true,
            max_concurrent: 4,
            max_job_timeout_s: 3600,
            labels: Vec::new(),
        }
    }
}

/// Partial CP settings (`PATCH`). `None` leaves the column unchanged.
#[derive(Debug, Clone, Default)]
pub struct HostCpSettings {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    pub paused: Option<bool>,
    pub drain: Option<bool>,
    pub allow_host: Option<bool>,
    pub allow_container: Option<bool>,
    pub max_concurrent: Option<i32>,
    pub max_job_timeout_s: Option<i32>,
    pub labels: Option<Vec<String>>,
}

/// Pair-token row (`worker_host_pair_tokens`). Store only `token_hash`.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WorkerHostPairToken {
    pub id: Uuid,
    pub host_id: Uuid,
    pub workspace_id: Uuid,
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}
