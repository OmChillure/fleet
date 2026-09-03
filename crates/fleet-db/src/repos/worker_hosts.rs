//! Paired worker hosts — create-then-pair tokens, machine auth, heartbeat.

use chrono::{DateTime, Duration, Utc};
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::error::{DbError, DbResult};
use crate::models::{
    HostCpSettings, NewWorkerHost, WORKER_HOST_ISOLATION_SANDBOXED, WORKER_HOST_OFFLINE_AFTER,
    WORKER_HOST_STATUS_DISABLED, WORKER_HOST_STATUS_ONLINE, WORKER_HOST_STATUS_PENDING, WorkerHost,
    WorkerHostPairToken, parse_worker_host_isolation,
};
use crate::pool::map_query_err;

pub const PAIR_TOKEN_PREFIX: &str = "flp_";
pub const MACHINE_TOKEN_PREFIX: &str = "flm_";

const HOST_COLS: &str = r#"
    id, workspace_id, name, status, enabled, paused, drain, isolation,
    allow_host, allow_container, max_concurrent, max_job_timeout_s, labels,
    machine_token_hash, advertised_host, advertised_container, platform, hostname,
    last_seen_at, last_error, current_jobs, advertised_max_concurrent,
    software_git_sha, software_bins, software_reported_at, software_updating, auto_update,
    update_requested_at, machine_id,
    hardware, agent_runtimes, settings_gen, settings_ack_gen,
    install_request, install_requested_at,
    runtime_settings, runtime_reported,
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
    let isolation = match patch.isolation.as_deref() {
        None => current.isolation.as_str(),
        Some(raw) => parse_worker_host_isolation(raw)
            .ok_or_else(|| DbError::Conflict("isolation must be native or sandboxed".into()))?,
    };
    let allow_host = patch.allow_host.unwrap_or(current.allow_host);
    let allow_container = patch.allow_container.unwrap_or(current.allow_container);
    let max_concurrent = patch.max_concurrent.unwrap_or(current.max_concurrent);
    let max_job_timeout_s = patch.max_job_timeout_s.unwrap_or(current.max_job_timeout_s);
    let labels = patch.labels.as_ref().unwrap_or(&current.labels);
    let auto_update = patch.auto_update.unwrap_or(current.auto_update);
    let update_requested_at = if patch.request_update == Some(true) {
        Some(Utc::now())
    } else {
        current.update_requested_at
    };
    let (install_request, install_requested_at) = if let Some(spec) = patch.install_model.as_ref() {
        (Some(spec.clone()), Some(Utc::now()))
    } else {
        (
            current.install_request.clone(),
            current.install_requested_at,
        )
    };
    let runtime_settings = if let Some(incoming) = patch.runtime_settings.as_ref() {
        merge_json_objects(&current.runtime_settings, incoming)
    } else {
        current.runtime_settings.clone()
    };
    let bump = name.as_str() != current.name.as_str()
        || enabled != current.enabled
        || paused != current.paused
        || drain != current.drain
        || isolation != current.isolation.as_str()
        || allow_host != current.allow_host
        || allow_container != current.allow_container
        || max_concurrent != current.max_concurrent
        || max_job_timeout_s != current.max_job_timeout_s
        || labels != &current.labels
        || auto_update != current.auto_update
        || runtime_settings != current.runtime_settings;
    let settings_gen = if bump {
        current.settings_gen.saturating_add(1)
    } else {
        current.settings_gen
    };

    let q = format!(
        r#"
        UPDATE worker_hosts
        SET name = $2,
            enabled = $3,
            paused = $4,
            drain = $5,
            isolation = $6,
            allow_host = $7,
            allow_container = $8,
            max_concurrent = $9,
            max_job_timeout_s = $10,
            labels = $11,
            auto_update = $12,
            update_requested_at = $13,
            settings_gen = $14,
            install_request = $15,
            install_requested_at = $16,
            runtime_settings = $17,
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
        .bind(isolation)
        .bind(allow_host)
        .bind(allow_container)
        .bind(max_concurrent)
        .bind(max_job_timeout_s)
        .bind(labels)
        .bind(auto_update)
        .bind(update_requested_at)
        .bind(settings_gen)
        .bind(install_request)
        .bind(install_requested_at)
        .bind(runtime_settings)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)?
        .ok_or_else(|| DbError::NotFound("worker host not found".into()))
}

fn merge_json_objects(base: &serde_json::Value, incoming: &serde_json::Value) -> serde_json::Value {
    let mut out = base.as_object().cloned().unwrap_or_default();
    if let Some(map) = incoming.as_object() {
        for (k, v) in map {
            if v.is_null() {
                out.remove(k);
            } else {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    serde_json::Value::Object(out)
}

pub async fn set_runtime_reported(
    pool: &PgPool,
    host_id: Uuid,
    runtime: serde_json::Value,
) -> DbResult<()> {
    sqlx::query(
        r#"
        UPDATE worker_hosts
        SET runtime_reported = $2, updated_at = now()
        WHERE id = $1
        "#,
    )
    .bind(host_id)
    .bind(runtime)
    .execute(pool)
    .await
    .map_err(map_query_err)?;
    Ok(())
}

pub async fn clear_install_request(pool: &PgPool, host_id: Uuid) -> DbResult<WorkerHost> {
    clear_install_request_if(pool, host_id, None).await
}

/// Clear only when `ack_spec` is omitted (old host) or matches the queued spec.
pub async fn clear_install_request_if(
    pool: &PgPool,
    host_id: Uuid,
    ack_spec: Option<&str>,
) -> DbResult<WorkerHost> {
    let q = format!(
        r#"
        UPDATE worker_hosts
        SET install_request = NULL,
            install_requested_at = NULL,
            updated_at = now()
        WHERE id = $1
          AND (
            $2::text IS NULL
            OR install_request IS NULL
            OR COALESCE(install_request->>'spec', '') = ''
            OR install_request->>'spec' = $2
          )
        RETURNING {HOST_COLS}
        "#
    );
    sqlx::query_as::<_, WorkerHost>(&q)
        .bind(host_id)
        .bind(ack_spec)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)?
        .ok_or_else(|| DbError::NotFound("worker host not found".into()))
}

pub async fn ack_settings(pool: &PgPool, host_id: Uuid, ack_gen: i32) -> DbResult<WorkerHost> {
    let q = format!(
        r#"
        UPDATE worker_hosts
        SET settings_ack_gen = $2,
            updated_at = now()
        WHERE id = $1
          AND settings_gen = $2
        RETURNING {HOST_COLS}
        "#
    );
    sqlx::query_as::<_, WorkerHost>(&q)
        .bind(host_id)
        .bind(ack_gen)
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
    machine_id: Option<Uuid>,
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

    // Same box again: machine.id always reclaims. Hostname+platform only
    // when the binary omitted machine.id, and only a disabled or stale row —
    // a live same-hostname box must not have its token rotated.
    if let Some(prior) = find_reclaim_target(
        &mut tx,
        host.workspace_id,
        host.id,
        machine_id,
        hostname,
        platform,
    )
    .await?
    {
        let mid = machine_id.or(prior.machine_id).unwrap_or(prior.id);
        return reclaim_existing(
            tx,
            pair,
            host,
            prior,
            hostname,
            platform,
            advertised_host,
            advertised_container,
            mid,
        )
        .await;
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
            machine_id = $8,
            paired_at = now(),
            last_seen_at = now(),
            last_error = NULL,
            updated_at = now()
        WHERE id = $1
        RETURNING {HOST_COLS}
        "#
    );
    let assigned_machine_id = machine_id.unwrap_or(host.id);
    let host = sqlx::query_as::<_, WorkerHost>(&upd)
        .bind(host.id)
        .bind(display_name)
        .bind(&machine_token_hash)
        .bind(hostname)
        .bind(platform)
        .bind(advertised_host)
        .bind(advertised_container)
        .bind(assigned_machine_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_query_err)?;

    tx.commit().await.map_err(DbError::from)?;
    Ok(PairedWorkerHost {
        host,
        machine_token,
    })
}

async fn find_reclaim_target(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    workspace_id: Uuid,
    exclude_id: Uuid,
    machine_id: Option<Uuid>,
    hostname: &str,
    platform: &str,
) -> DbResult<Option<WorkerHost>> {
    if let Some(mid) = machine_id {
        let q = format!(
            r#"
            SELECT {HOST_COLS}
            FROM worker_hosts
            WHERE workspace_id = $1 AND machine_id = $2 AND id <> $3
            FOR UPDATE
            "#
        );
        return sqlx::query_as::<_, WorkerHost>(&q)
            .bind(workspace_id)
            .bind(mid)
            .bind(exclude_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(map_query_err);
    }

    let hostname = hostname.trim();
    let platform = platform.trim();
    if hostname.is_empty() || platform.is_empty() {
        return Ok(None);
    }
    // Prefer a revoked row; otherwise only a box that already looks offline.
    let stale_before = Utc::now() - WORKER_HOST_OFFLINE_AFTER;
    let q = format!(
        r#"
        SELECT {HOST_COLS}
        FROM worker_hosts
        WHERE workspace_id = $1
          AND id <> $2
          AND hostname IS NOT NULL
          AND lower(trim(hostname)) = lower($3)
          AND platform IS NOT NULL
          AND lower(trim(platform)) = lower($4)
          AND (
            status = '{WORKER_HOST_STATUS_DISABLED}'
            OR last_seen_at IS NULL
            OR last_seen_at < $5
          )
        ORDER BY
          CASE WHEN status = '{WORKER_HOST_STATUS_DISABLED}' THEN 0 ELSE 1 END,
          last_seen_at ASC NULLS FIRST,
          paired_at ASC NULLS LAST,
          created_at ASC
        LIMIT 1
        FOR UPDATE
        "#
    );
    sqlx::query_as::<_, WorkerHost>(&q)
        .bind(workspace_id)
        .bind(exclude_id)
        .bind(hostname)
        .bind(platform)
        .bind(stale_before)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_query_err)
}

async fn reclaim_existing(
    mut tx: sqlx::Transaction<'_, sqlx::Postgres>,
    pair: WorkerHostPairToken,
    pending: WorkerHost,
    prior: WorkerHost,
    hostname: &str,
    platform: &str,
    advertised_host: bool,
    advertised_container: bool,
    machine_id: Uuid,
) -> DbResult<PairedWorkerHost> {
    sqlx::query(
        r#"
        UPDATE worker_host_pair_tokens
        SET used_at = now()
        WHERE id = $1 OR (host_id = $2 AND used_at IS NULL)
        "#,
    )
    .bind(pair.id)
    .bind(pending.id)
    .execute(&mut *tx)
    .await
    .map_err(map_query_err)?;

    sqlx::query(
        r#"
        UPDATE worker_hosts
        SET status = $2,
            enabled = FALSE,
            updated_at = now()
        WHERE id = $1
        "#,
    )
    .bind(pending.id)
    .bind(WORKER_HOST_STATUS_DISABLED)
    .execute(&mut *tx)
    .await
    .map_err(map_query_err)?;

    let machine_token = generate_machine_token();
    let machine_token_hash = hash_token(&machine_token);
    let display_name = if prior.name.trim().is_empty() {
        hostname.to_string()
    } else {
        prior.name.clone()
    };
    let upd = format!(
        r#"
        UPDATE worker_hosts
        SET status = '{WORKER_HOST_STATUS_ONLINE}',
            enabled = TRUE,
            name = $2,
            machine_token_hash = $3,
            hostname = $4,
            platform = $5,
            advertised_host = $6,
            advertised_container = $7,
            machine_id = $8,
            paired_at = now(),
            last_seen_at = now(),
            last_error = NULL,
            updated_at = now()
        WHERE id = $1
        RETURNING {HOST_COLS}
        "#
    );
    let host = sqlx::query_as::<_, WorkerHost>(&upd)
        .bind(prior.id)
        .bind(display_name)
        .bind(&machine_token_hash)
        .bind(hostname)
        .bind(platform)
        .bind(advertised_host)
        .bind(advertised_container)
        .bind(machine_id)
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
    heartbeat_with_machine(
        pool,
        host_id,
        advertised_host,
        advertised_container,
        platform,
        hostname,
        last_error,
        current_jobs,
        advertised_max_concurrent,
        software_git_sha,
        software_bins,
        software_updating,
        hardware,
        None,
        None,
    )
    .await
}

pub async fn heartbeat_with_machine(
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
    machine_id: Option<Uuid>,
    agent_runtimes: Option<serde_json::Value>,
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
            hardware = COALESCE($13, hardware),
            agent_runtimes = COALESCE($15, agent_runtimes),
            machine_id = CASE
                WHEN $14::uuid IS NULL THEN COALESCE(machine_id, id)
                WHEN EXISTS (
                    SELECT 1 FROM worker_hosts o
                    WHERE o.workspace_id = worker_hosts.workspace_id
                      AND o.machine_id = $14
                      AND o.id <> worker_hosts.id
                ) THEN COALESCE(machine_id, id)
                ELSE $14
            END
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
        .bind(machine_id)
        .bind(agent_runtimes)
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

/// Host already paired on this workspace with this durable box id.
pub async fn find_by_machine_id(
    pool: &PgPool,
    workspace_id: Uuid,
    machine_id: Uuid,
) -> DbResult<Option<WorkerHost>> {
    let q = format!(
        r#"
        SELECT {HOST_COLS}
        FROM worker_hosts
        WHERE workspace_id = $1 AND machine_id = $2
        ORDER BY paired_at ASC NULLS LAST, created_at ASC, id ASC
        LIMIT 1
        "#
    );
    sqlx::query_as::<_, WorkerHost>(&q)
        .bind(workspace_id)
        .bind(machine_id)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)
}

/// Issue a new machine token (cluster-host restart without a persisted token).
pub async fn rotate_machine_token(pool: &PgPool, host_id: Uuid) -> DbResult<PairedWorkerHost> {
    let machine_token = generate_machine_token();
    let machine_token_hash = hash_token(&machine_token);
    let q = format!(
        r#"
        UPDATE worker_hosts
        SET machine_token_hash = $2,
            status = '{WORKER_HOST_STATUS_ONLINE}',
            last_seen_at = now(),
            last_error = NULL,
            updated_at = now()
        WHERE id = $1
        RETURNING {HOST_COLS}
        "#
    );
    let host = sqlx::query_as::<_, WorkerHost>(&q)
        .bind(host_id)
        .bind(&machine_token_hash)
        .fetch_optional(pool)
        .await
        .map_err(map_query_err)?
        .ok_or_else(|| DbError::NotFound("worker host not found".into()))?;
    Ok(PairedWorkerHost {
        host,
        machine_token,
    })
}

/// In-cluster bootstrap boxes (`labels` contains `cluster`), any workspace.
pub async fn list_cluster(pool: &PgPool) -> DbResult<Vec<WorkerHost>> {
    let q = format!(
        r#"
        SELECT {HOST_COLS}
        FROM worker_hosts
        WHERE 'cluster' = ANY (labels)
        ORDER BY last_seen_at DESC NULLS LAST, id ASC
        "#
    );
    sqlx::query_as::<_, WorkerHost>(&q)
        .fetch_all(pool)
        .await
        .map_err(map_query_err)
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
        WHERE (workspace_id = $1 OR 'cluster' = ANY(labels))
          AND enabled
          AND NOT paused
          AND NOT drain
          AND isolation <> '{WORKER_HOST_ISOLATION_SANDBOXED}'
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

/// Paired hosts mid host-bin restart (`software_updating`) seen within `seen_within`.
/// Tick uses this so a brief restart does not assign elsewhere while the
/// paired host is swapping its binary. No host → task stays ready.
/// Does not change pause/drain meaning.
pub async fn list_updating_recent(
    pool: &PgPool,
    workspace_id: Uuid,
    seen_within: Duration,
) -> DbResult<Vec<WorkerHost>> {
    let cutoff = Utc::now() - seen_within;
    let q = format!(
        r#"
        SELECT {HOST_COLS}
        FROM worker_hosts
        WHERE workspace_id = $1
          AND software_updating
          AND enabled
          AND status <> '{WORKER_HOST_STATUS_DISABLED}'
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
