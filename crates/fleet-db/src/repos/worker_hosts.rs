//! Paired worker hosts — create-then-pair tokens, machine auth, heartbeat.

use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::models::{
    HostCpSettings, NewWorkerHost, WORKER_HOST_STATUS_DISABLED, WORKER_HOST_STATUS_ONLINE,
    WORKER_HOST_STATUS_PENDING, WorkerHost, WorkerHostPairToken,
};
use crate::pool::map_query_err;

pub const PAIR_TOKEN_PREFIX: &str = "flp_";
pub const MACHINE_TOKEN_PREFIX: &str = "flm_";

const HOST_COLS: &str = r#"
    id, workspace_id, name, status, enabled, paused, drain,
    allow_host, allow_container, max_concurrent, max_job_timeout_s, labels,
    machine_token_hash, advertised_host, advertised_container, platform, hostname,
    last_seen_at, last_error, current_jobs, advertised_max_concurrent,
    software_git_sha, software_bins, software_reported_at, software_updating, auto_update,
    hardware,
    created_by, created_at, paired_at, updated_at
"#;

const PAIR_TOKEN_COLS: &str = r#"
    id, host_id, workspace_id, token_hash, expires_at, used_at, created_at
"#;

/// `flp_` + 32 random bytes as hex.
pub fn generate_pair_token() -> String {
    generate_prefixed_token(PAIR_TOKEN_PREFIX)
}

/// `flm_` + 32 random bytes as hex.
pub fn generate_machine_token() -> String {
    generate_prefixed_token(MACHINE_TOKEN_PREFIX)
}

pub fn hash_token(token: &str) -> String {
    let mut h = Sha256::new();
    h.update(token.as_bytes());
    hex::encode(h.finalize())
}

pub fn verify_token(token: &str, hash_hex: &str) -> bool {
    let got = hash_token(token);
    got.as_bytes().ct_eq(hash_hex.as_bytes()).into()
}

fn generate_prefixed_token(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("{prefix}{}", hex::encode(bytes))
}

/// Successful pair: host is `online` and the plaintext machine token is shown once.
#[derive(Debug, Clone)]
pub struct PairedWorkerHost {
    pub host: WorkerHost,
    pub machine_token: String,
}

pub async fn create_pending(
    pool: &PgPool,
    new: &NewWorkerHost,
    pair_token_hash: &str,
    expires_at: DateTime<Utc>,
) -> DbResult<WorkerHost> {
    let mut tx = pool.begin().await.map_err(DbError::from)?;
    let q = format!(
        r#"
        INSERT INTO worker_hosts (
            id, workspace_id, name, status,
            allow_host, allow_container, max_concurrent, max_job_timeout_s, labels,
            created_by
        )
        VALUES ($1, $2, $3, '{WORKER_HOST_STATUS_PENDING}', $4, $5, $6, $7, $8, $9)
        RETURNING {HOST_COLS}
        "#
    );
    let host = sqlx::query_as::<_, WorkerHost>(&q)
        .bind(new.id)
        .bind(new.workspace_id)
        .bind(&new.name)
        .bind(new.allow_host)
        .bind(new.allow_container)
        .bind(new.max_concurrent)
        .bind(new.max_job_timeout_s)
        .bind(&new.labels)
        .bind(new.created_by)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_query_err)?;

    sqlx::query(
        r#"
        INSERT INTO worker_host_pair_tokens (
            id, host_id, workspace_id, token_hash, expires_at
        )
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(host.id)
    .bind(host.workspace_id)
    .bind(pair_token_hash)
    .bind(expires_at)
    .execute(&mut *tx)
    .await
    .map_err(map_query_err)?;

    tx.commit().await.map_err(DbError::from)?;
    Ok(host)
}

pub async fn get(pool: &PgPool, host_id: Uuid) -> DbResult<Option<WorkerHost>> {
    let q = format!("SELECT {HOST_COLS} FROM worker_hosts WHERE id = $1");
    sqlx::query_as::<_, WorkerHost>(&q)
        .bind(host_id)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn get_in_workspace(
    pool: &PgPool,
    workspace_id: Uuid,
    host_id: Uuid,
) -> DbResult<Option<WorkerHost>> {
    let q = format!(
        r#"
        SELECT {HOST_COLS}
        FROM worker_hosts
        WHERE workspace_id = $1 AND id = $2
        "#
    );
    sqlx::query_as::<_, WorkerHost>(&q)
        .bind(workspace_id)
        .bind(host_id)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

pub async fn list_for_workspace(pool: &PgPool, workspace_id: Uuid) -> DbResult<Vec<WorkerHost>> {
    let q = format!(
        r#"
        SELECT {HOST_COLS}
        FROM worker_hosts
        WHERE workspace_id = $1
        ORDER BY created_at ASC, id ASC
        "#
    );
    sqlx::query_as::<_, WorkerHost>(&q)
        .bind(workspace_id)
        .fetch_all(pool)
        .await
        .map_err(map_query_err)
}

pub async fn patch_cp_settings(
    pool: &PgPool,
    host_id: Uuid,
    patch: &HostCpSettings,
) -> DbResult<WorkerHost> {
    let current = get(pool, host_id)
        .await?
        .ok_or_else(|| DbError::NotFound("worker host not found".into()))?;

    let name = patch.name.as_ref().unwrap_or(&current.name);
    let enabled = patch.enabled.unwrap_or(current.enabled);
    let paused = patch.paused.unwrap_or(current.paused);
    let drain = patch.drain.unwrap_or(current.drain);
    let allow_host = patch.allow_host.unwrap_or(current.allow_host);
    let allow_container = patch.allow_container.unwrap_or(current.allow_container);
    let max_concurrent = patch.max_concurrent.unwrap_or(current.max_concurrent);
    let max_job_timeout_s = patch.max_job_timeout_s.unwrap_or(current.max_job_timeout_s);
    let labels = patch.labels.as_ref().unwrap_or(&current.labels);

    let q = format!(
        r#"
        UPDATE worker_hosts
        SET name = $2,
            enabled = $3,
            paused = $4,
            drain = $5,
            allow_host = $6,
            allow_container = $7,
            max_concurrent = $8,
            max_job_timeout_s = $9,
            labels = $10,
            updated_at = now()
        WHERE id = $1
        RETURNING {HOST_COLS}
        "#
    );
    sqlx::query_as::<_, WorkerHost>(&q)
        .bind(host_id)
        .bind(name)
        .bind(enabled)
        .bind(paused)
        .bind(drain)
        .bind(allow_host)
        .bind(allow_container)
        .bind(max_concurrent)
        .bind(max_job_timeout_s)
        .bind(labels)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)?
        .ok_or_else(|| DbError::NotFound("worker host not found".into()))
}

pub async fn consume_pair_token(
    pool: &PgPool,
    token_plaintext: &str,
    hostname: &str,
    platform: &str,
    advertised_host: bool,
    advertised_container: bool,
) -> DbResult<PairedWorkerHost> {
    let token_hash = hash_token(token_plaintext);
    let mut tx = pool.begin().await.map_err(DbError::from)?;

    let pair_q = format!(
        r#"
        SELECT {PAIR_TOKEN_COLS}
        FROM worker_host_pair_tokens
        WHERE token_hash = $1
        FOR UPDATE
        "#
    );
    let pair = sqlx::query_as::<_, WorkerHostPairToken>(&pair_q)
        .bind(&token_hash)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_query_err)?
        .ok_or_else(|| DbError::NotFound("pair token not found".into()))?;

    if pair.used_at.is_some() {
        return Err(DbError::Conflict("pair token already used".into()));
    }
    if pair.expires_at <= Utc::now() {
        return Err(DbError::Conflict("pair token expired".into()));
    }

    let host_q = format!(
        r#"
        SELECT {HOST_COLS}
        FROM worker_hosts
        WHERE id = $1
        FOR UPDATE
        "#
    );
    let host = sqlx::query_as::<_, WorkerHost>(&host_q)
        .bind(pair.host_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_query_err)?
        .ok_or_else(|| DbError::NotFound("worker host not found".into()))?;

    if host.status != WORKER_HOST_STATUS_PENDING {
        return Err(DbError::Conflict("host is not pending".into()));
    }

    sqlx::query(
        r#"
        UPDATE worker_host_pair_tokens
        SET used_at = now()
        WHERE id = $1
        "#,
    )
    .bind(pair.id)
    .execute(&mut *tx)
    .await
    .map_err(map_query_err)?;

    // Leftover unused tokens cannot pair once the host leaves pending; mark them used.
    sqlx::query(
        r#"
        UPDATE worker_host_pair_tokens
        SET used_at = now()
        WHERE host_id = $1 AND used_at IS NULL AND id <> $2
        "#,
    )
    .bind(host.id)
    .bind(pair.id)
    .execute(&mut *tx)
    .await
    .map_err(map_query_err)?;

    let machine_token = generate_machine_token();
    let machine_token_hash = hash_token(&machine_token);
    let display_name = if host.name.trim().is_empty() {
        hostname
    } else {
        host.name.as_str()
    };

    let upd = format!(
        r#"
        UPDATE worker_hosts
        SET status = '{WORKER_HOST_STATUS_ONLINE}',
            name = $2,
            machine_token_hash = $3,
            hostname = $4,
            platform = $5,
            advertised_host = $6,
            advertised_container = $7,
            paired_at = now(),
            last_seen_at = now(),
            last_error = NULL,
            updated_at = now()
        WHERE id = $1
        RETURNING {HOST_COLS}
        "#
    );
    let host = sqlx::query_as::<_, WorkerHost>(&upd)
        .bind(host.id)
        .bind(display_name)
        .bind(&machine_token_hash)
        .bind(hostname)
        .bind(platform)
        .bind(advertised_host)
        .bind(advertised_container)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_query_err)?;

    tx.commit().await.map_err(DbError::from)?;
    Ok(PairedWorkerHost {
        host,
        machine_token,
    })
}

pub async fn authenticate_machine(
    pool: &PgPool,
    token_plaintext: &str,
) -> DbResult<Option<WorkerHost>> {
    let token_hash = hash_token(token_plaintext);
    let q = format!(
        r#"
        SELECT {HOST_COLS}
        FROM worker_hosts
        WHERE machine_token_hash = $1
        "#
    );
    let host = sqlx::query_as::<_, WorkerHost>(&q)
        .bind(&token_hash)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)?;

    let Some(host) = host else {
        return Ok(None);
    };
    let Some(stored) = host.machine_token_hash.as_deref() else {
        return Ok(None);
    };
    if host.status == WORKER_HOST_STATUS_DISABLED || !verify_token(token_plaintext, stored) {
        return Ok(None);
    }
    Ok(Some(host))
}

pub async fn heartbeat(
    pool: &PgPool,
    host_id: Uuid,
    advertised_host: bool,
    advertised_container: bool,
    platform: Option<&str>,
    hostname: Option<&str>,
    last_error: Option<&str>,
    current_jobs: i32,
    advertised_max_concurrent: Option<i32>,
    software_git_sha: Option<&str>,
    software_bins: Option<serde_json::Value>,
    software_updating: Option<bool>,
    hardware: Option<serde_json::Value>,
) -> DbResult<WorkerHost> {
    let current_jobs = current_jobs.max(0);
    let advertised_max_concurrent = advertised_max_concurrent.map(|n| n.clamp(1, 256));
    let report_software = software_git_sha.is_some() || software_bins.is_some();
    let q = format!(
        r#"
        UPDATE worker_hosts
        SET advertised_host = $2,
            advertised_container = $3,
            platform = COALESCE($4, platform),
            hostname = COALESCE($5, hostname),
            last_error = $6,
            last_seen_at = now(),
            updated_at = now(),
            current_jobs = $7,
            advertised_max_concurrent = COALESCE($8, advertised_max_concurrent),
            software_git_sha = COALESCE($9, software_git_sha),
            software_bins = COALESCE($10, software_bins),
            software_reported_at = CASE WHEN $11 THEN now() ELSE software_reported_at END,
            software_updating = COALESCE($12, software_updating),
            hardware = COALESCE($13, hardware)
        WHERE id = $1
          AND status <> '{WORKER_HOST_STATUS_DISABLED}'
        RETURNING {HOST_COLS}
        "#
    );
    sqlx::query_as::<_, WorkerHost>(&q)
        .bind(host_id)
        .bind(advertised_host)
        .bind(advertised_container)
        .bind(platform)
        .bind(hostname)
        .bind(last_error)
        .bind(current_jobs)
        .bind(advertised_max_concurrent)
        .bind(software_git_sha)
        .bind(software_bins)
        .bind(report_software)
        .bind(software_updating)
        .bind(hardware)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)?
        .ok_or_else(|| DbError::NotFound("worker host not found".into()))
}

pub async fn revoke(pool: &PgPool, host_id: Uuid) -> DbResult<WorkerHost> {
    let mut tx = pool.begin().await.map_err(DbError::from)?;
    let q = format!(
        r#"
        UPDATE worker_hosts
        SET status = '{WORKER_HOST_STATUS_DISABLED}',
            machine_token_hash = NULL,
            updated_at = now()
        WHERE id = $1
        RETURNING {HOST_COLS}
        "#
    );
    let host = sqlx::query_as::<_, WorkerHost>(&q)
        .bind(host_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_query_err)?
        .ok_or_else(|| DbError::NotFound("worker host not found".into()))?;

    sqlx::query(
        r#"
        UPDATE worker_host_pair_tokens
        SET used_at = COALESCE(used_at, now())
        WHERE host_id = $1 AND used_at IS NULL
        "#,
    )
    .bind(host_id)
    .execute(&mut *tx)
    .await
    .map_err(map_query_err)?;

    tx.commit().await.map_err(DbError::from)?;
    Ok(host)
}

/// Issue a new pair token only while the host is still `pending`.
pub async fn replace_pair_token(
    pool: &PgPool,
    host_id: Uuid,
    pair_token_hash: &str,
    expires_at: DateTime<Utc>,
) -> DbResult<WorkerHost> {
    let mut tx = pool.begin().await.map_err(DbError::from)?;
    let host_q = format!(
        r#"
        SELECT {HOST_COLS}
        FROM worker_hosts
        WHERE id = $1
        FOR UPDATE
        "#
    );
    let host = sqlx::query_as::<_, WorkerHost>(&host_q)
        .bind(host_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_query_err)?
        .ok_or_else(|| DbError::NotFound("worker host not found".into()))?;

    if host.status != WORKER_HOST_STATUS_PENDING {
        return Err(DbError::Conflict(
            "pair token can only be replaced while host is pending".into(),
        ));
    }

    sqlx::query(
        r#"
        UPDATE worker_host_pair_tokens
        SET used_at = now()
        WHERE host_id = $1 AND used_at IS NULL
        "#,
    )
    .bind(host_id)
    .execute(&mut *tx)
    .await
    .map_err(map_query_err)?;

    sqlx::query(
        r#"
        INSERT INTO worker_host_pair_tokens (
            id, host_id, workspace_id, token_hash, expires_at
        )
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(host.id)
    .bind(host.workspace_id)
    .bind(pair_token_hash)
    .bind(expires_at)
    .execute(&mut *tx)
    .await
    .map_err(map_query_err)?;

    tx.commit().await.map_err(DbError::from)?;
    Ok(host)
}

/// Tick candidate set: enabled, not paused/drain, stored `online`, seen recently, paired.
pub async fn list_assignable(
    pool: &PgPool,
    workspace_id: Uuid,
    stale_after: Duration,
) -> DbResult<Vec<WorkerHost>> {
    let cutoff = Utc::now() - stale_after;
    let q = format!(
        r#"
        SELECT {HOST_COLS}
        FROM worker_hosts
        WHERE workspace_id = $1
          AND enabled
          AND NOT paused
          AND NOT drain
          AND status = '{WORKER_HOST_STATUS_ONLINE}'
          AND machine_token_hash IS NOT NULL
          AND last_seen_at IS NOT NULL
          AND last_seen_at >= $2
        ORDER BY last_seen_at DESC, id ASC
        "#
    );
    sqlx::query_as::<_, WorkerHost>(&q)
        .bind(workspace_id)
        .bind(cutoff)
        .fetch_all(pool)
        .await
        .map_err(map_query_err)
}

#[cfg(test)]
mod token_tests {
    use super::*;

    #[test]
    fn pair_and_machine_token_prefixes_and_hash_roundtrip() {
        let pair = generate_pair_token();
        let machine = generate_machine_token();
        assert!(pair.starts_with(PAIR_TOKEN_PREFIX), "{pair}");
        assert!(machine.starts_with(MACHINE_TOKEN_PREFIX), "{machine}");
        assert_eq!(pair.len(), PAIR_TOKEN_PREFIX.len() + 64);
        assert_eq!(machine.len(), MACHINE_TOKEN_PREFIX.len() + 64);
        assert_ne!(pair, machine);

        let h = hash_token(&pair);
        assert_eq!(h.len(), 64);
        assert!(verify_token(&pair, &h));
        assert!(!verify_token(&machine, &h));
        assert!(!verify_token(&pair, "deadbeef"));
    }
}
