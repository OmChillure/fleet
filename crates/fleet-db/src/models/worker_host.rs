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
    pub software_git_sha: Option<String>,
    pub software_bins: Option<serde_json::Value>,
    pub software_reported_at: Option<DateTime<Utc>>,
    pub software_updating: bool,
    pub auto_update: bool,
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
    #[serde(default)]
    pub software: Option<WorkerSoftwarePublic>,
    #[serde(default = "default_auto_update")]
    pub auto_update: bool,
}

fn default_auto_update() -> bool {
    true
}

/// What the box last reported. `None` on `WorkerHostPublic.software` means never reported.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkerSoftwarePublic {
    pub git_sha: Option<String>,
    #[serde(default)]
    pub bins: Option<serde_json::Value>,
    pub reported_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub updating: bool,
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
            software: if host.software_reported_at.is_some() || host.software_git_sha.is_some() {
                Some(WorkerSoftwarePublic {
                    git_sha: host.software_git_sha.clone(),
                    bins: host.software_bins.clone(),
                    reported_at: host.software_reported_at,
                    updating: host.software_updating,
                })
            } else {
                None
            },
            auto_update: host.auto_update,
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

#[cfg(test)]
mod public_view_tests {
    use super::*;

    fn host(status: &str, last_seen: Option<DateTime<Utc>>) -> WorkerHost {
        let now = Utc::now();
        WorkerHost {
            id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            name: "studio".into(),
            status: status.into(),
            enabled: true,
            paused: false,
            drain: false,
            allow_host: true,
            allow_container: true,
            max_concurrent: 4,
            max_job_timeout_s: 3600,
            labels: vec![],
            machine_token_hash: Some("should-never-leave".into()),
            advertised_host: true,
            advertised_container: false,
            platform: Some("linux-x86_64".into()),
            hostname: Some("box".into()),
            last_seen_at: last_seen,
            last_error: None,
            current_jobs: 0,
            advertised_max_concurrent: None,
            software_git_sha: None,
            software_bins: None,
            software_reported_at: None,
            software_updating: false,
            auto_update: true,
            created_by: None,
            created_at: now,
            paired_at: Some(now),
            updated_at: now,
        }
    }

    #[test]
    fn stale_online_host_is_reported_offline() {
        let now = Utc::now();
        let fresh = host(WORKER_HOST_STATUS_ONLINE, Some(now - Duration::seconds(5)));
        assert_eq!(fresh.public_status(now), WORKER_HOST_STATUS_ONLINE);

        let stale = host(WORKER_HOST_STATUS_ONLINE, Some(now - Duration::seconds(31)));
        assert_eq!(stale.public_status(now), "offline");

        let never_seen = host(WORKER_HOST_STATUS_ONLINE, None);
        assert_eq!(never_seen.public_status(now), "offline");
    }

    #[test]
    fn pending_and_disabled_are_not_rewritten_to_offline() {
        let now = Utc::now();
        let pending = host(WORKER_HOST_STATUS_PENDING, None);
        assert_eq!(pending.public_status(now), WORKER_HOST_STATUS_PENDING);
        let disabled = host(WORKER_HOST_STATUS_DISABLED, Some(now - Duration::hours(1)));
        assert_eq!(disabled.public_status(now), WORKER_HOST_STATUS_DISABLED);
    }

    #[test]
    fn public_json_never_includes_hashes_or_tokens() {
        let h = host(WORKER_HOST_STATUS_ONLINE, Some(Utc::now()));
        let v = serde_json::to_value(h.as_public(Utc::now())).expect("json");
        let obj = v.as_object().expect("object");
        for secret in [
            "machine_token_hash",
            "pair_token",
            "pair_token_hash",
            "token_hash",
            "machine_token",
        ] {
            assert!(
                !obj.contains_key(secret),
                "public host leaked {secret}: {v}"
            );
        }
        // Stored WorkerHost must also skip the hash on accidental serialize.
        let raw = serde_json::to_value(&h).expect("host json");
        assert!(
            raw.get("machine_token_hash").is_none(),
            "WorkerHost serialize leaked hash: {raw}"
        );
        assert_eq!(v["status"], "online");
        assert!(
            v.get("software").unwrap().is_null(),
            "never-reported software must be null: {v}"
        );
    }
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
