//! Fail-closed Postgres tests for paired worker hosts.
//! Requires DATABASE_URL — never skips.

mod common;

use chrono::{Duration, Utc};
use fleet_db::error::DbError;
use fleet_db::models::{HostCpSettings, NewWorkClaim, NewWorkerHost};
use fleet_db::repos::users::NewUser;
use fleet_db::repos::worker_hosts::{
    self, generate_pair_token, hash_token, PAIR_TOKEN_PREFIX, PAIR_TTL,
};
use fleet_db::repos::{users, work_claims, workspaces};
use serde_json::json;
use uuid::Uuid;

use common::{connect_and_migrate, unique_email, unique_slug};

fn assignable_ids_in_workspace(
    assignable: &[fleet_db::models::WorkerHost],
    workspace_id: Uuid,
) -> Vec<Uuid> {
    assignable
        .iter()
        .filter(|h| h.workspace_id == workspace_id)
        .map(|h| h.id)
        .collect()
}

async fn seed_user(pool: &fleet_db::PgPool, email: &str) -> Uuid {
    let id = Uuid::now_v7();
    let now = Utc::now();
    let mut tx = pool.begin().await.expect("begin");
    let user = users::insert_tx(
        &mut tx,
        &NewUser {
            id,
            email: email.to_string(),
            email_normalized: email.to_lowercase(),
            display_name: None,
            password_hash: "test-hash".into(),
            created_at: now,
        },
    )
    .await
    .expect("insert user");
    tx.commit().await.expect("commit user");
    assert_eq!(user.id, id);
    id
}

/// Seed user + personal workspace; return (user_id, workspace_id).
async fn seed_user_with_workspace(pool: &fleet_db::PgPool) -> (Uuid, Uuid) {
    let email = unique_email();
    let user_id = seed_user(pool, &email).await;
    let workspace_id = Uuid::now_v7();
    let slug = unique_slug("ws");
    let now = Utc::now();
    workspaces::create_owned(pool, workspace_id, user_id, &slug, "Test WS", now)
        .await
        .expect("create workspace");
    (user_id, workspace_id)
}

async fn create_pending_with_token(
    pool: &fleet_db::PgPool,
    workspace_id: Uuid,
    created_by: Uuid,
    name: &str,
    expires_at: chrono::DateTime<Utc>,
) -> (fleet_db::models::WorkerHost, String) {
    let token = generate_pair_token();
    let host = worker_hosts::create_pending(
        pool,
        &NewWorkerHost::new(
            Uuid::now_v7(),
            workspace_id,
            name.to_string(),
            Some(created_by),
        ),
        &hash_token(&token),
        expires_at,
    )
    .await
    .expect("create_pending");
    (host, token)
}

async fn pair_host(
    pool: &fleet_db::PgPool,
    token: &str,
    hostname: &str,
) -> worker_hosts::PairedWorkerHost {
    worker_hosts::consume_pair_token(pool, token, hostname, "linux-x86_64", true, false, None)
        .await
        .expect("consume_pair_token")
}

/// `token_hash` + `used_at` for the row matching this plaintext.
async fn pair_token_hash_and_used_at(
    pool: &fleet_db::PgPool,
    host_id: Uuid,
    token: &str,
) -> (String, Option<chrono::DateTime<Utc>>) {
    sqlx::query_as(
        r#"
        SELECT token_hash, used_at
        FROM worker_host_pair_tokens
        WHERE host_id = $1 AND token_hash = $2
        "#,
    )
    .bind(host_id)
    .bind(hash_token(token))
    .fetch_one(pool)
    .await
    .expect("pair token row")
}

const PAIR_TOKEN_PLAINTEXT_COLUMNS: &[&str] = &[
    "token",
    "pair_token",
    "plaintext",
    "token_plaintext",
    "pair_token_plaintext",
    "pair_token_plain",
];

/// Catalog: store the hash only — jsonb of the row must not contain plaintext.
async fn assert_pair_token_hashed_at_rest(pool: &fleet_db::PgPool, host_id: Uuid, token: &str) {
    let (stored, _) = pair_token_hash_and_used_at(pool, host_id, token).await;
    assert_eq!(stored, hash_token(token));
    assert_ne!(stored, token);
    assert_eq!(stored.len(), 64);
    assert!(
        stored.chars().all(|c| c.is_ascii_hexdigit()),
        "token_hash must be sha256 hex: {stored}"
    );
    assert!(
        !stored.contains(token),
        "plaintext pair token must not appear in token_hash"
    );
    assert!(
        !stored.contains(PAIR_TOKEN_PREFIX),
        "token_hash must not embed {PAIR_TOKEN_PREFIX}: {stored}"
    );
    let blob: serde_json::Value = sqlx::query_scalar(
        r#"
        SELECT to_jsonb(t)
        FROM worker_host_pair_tokens t
        WHERE host_id = $1 AND token_hash = $2
        "#,
    )
    .bind(host_id)
    .bind(hash_token(token))
    .fetch_one(pool)
    .await
    .expect("pair token jsonb");
    let blob_text = blob.to_string();
    assert!(
        !blob_text.contains(token),
        "plaintext pair token must not appear at rest: {blob_text}"
    );
    let obj = blob.as_object().expect("pair token row object");
    assert_eq!(
        obj.get("token_hash").and_then(|v| v.as_str()),
        Some(stored.as_str())
    );
    for key in PAIR_TOKEN_PLAINTEXT_COLUMNS {
        match obj.get(*key) {
            None => {}
            Some(v) => assert!(
                v.is_null(),
                "plaintext column {key} must stay null: {blob_text}"
            ),
        }
    }
    let host_blob: String = sqlx::query_scalar(
        r#"
        SELECT to_jsonb(h)::text
        FROM worker_hosts h
        WHERE id = $1
        "#,
    )
    .bind(host_id)
    .fetch_one(pool)
    .await
    .expect("worker_hosts jsonb");
    assert!(
        !host_blob.contains(token),
        "plaintext pair token must not appear on worker_hosts: {host_blob}"
    );
}

/// Pair-token plaintext column must not exist, or every value for this host is NULL.
async fn assert_pair_token_plaintext_column_absent_or_null(pool: &fleet_db::PgPool, host_id: Uuid) {
    let nullable: String = sqlx::query_scalar(
        r#"
        SELECT is_nullable
        FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND table_name = 'worker_host_pair_tokens'
          AND column_name = 'token_hash'
        "#,
    )
    .fetch_one(pool)
    .await
    .expect("token_hash column");
    assert_eq!(nullable, "NO", "token_hash must be NOT NULL");

    let cols: Vec<(String, String)> = sqlx::query_as(
        r#"
        SELECT table_name, column_name
        FROM information_schema.columns
        WHERE table_schema = current_schema()
          AND (
            (
              table_name = 'worker_host_pair_tokens'
              AND column_name IN (
                'token', 'pair_token', 'plaintext', 'token_plaintext',
                'pair_token_plaintext', 'pair_token_plain'
              )
            )
            OR (
              table_name = 'worker_hosts'
              AND column_name IN (
                'token', 'pair_token', 'plaintext', 'token_plaintext',
                'pair_token_plaintext', 'pair_token_plain', 'pair_token_hash'
              )
            )
          )
        ORDER BY table_name, column_name
        "#,
    )
    .fetch_all(pool)
    .await
    .expect("information_schema pair token columns");

    for (table, col) in &cols {
        assert!(
            table.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
                && col.chars().all(|c| c == '_' || c.is_ascii_alphanumeric()),
            "unexpected identifier {table}.{col}"
        );
        let filter = if table.as_str() == "worker_host_pair_tokens" {
            "host_id"
        } else {
            "id"
        };
        let sql = format!(
            "SELECT COUNT(*)::bigint FROM {table} WHERE {filter} = $1 AND {col} IS NOT NULL"
        );
        let n: i64 = sqlx::query_scalar(&sql)
            .bind(host_id)
            .fetch_one(pool)
            .await
            .unwrap_or_else(|e| panic!("select {table}.{col}: {e}"));
        assert_eq!(
            n, 0,
            "plaintext pair token column {table}.{col} must stay null for host {host_id}"
        );
    }
}

fn assert_repo_host_omits_pair_plaintext(host: &fleet_db::models::WorkerHost, tokens: &[&str]) {
    let raw = serde_json::to_value(host).expect("WorkerHost json");
    let public = serde_json::to_value(host.as_public(Utc::now())).expect("as_public json");
    let dbg = format!("{host:?}");
    let blobs = [
        ("WorkerHost serde", raw.to_string()),
        ("as_public", public.to_string()),
        ("Debug", dbg),
    ];
    for token in tokens {
        assert!(
            token.starts_with(PAIR_TOKEN_PREFIX),
            "test token must be flp_ plaintext, got {token}"
        );
        for (label, blob) in &blobs {
            assert!(
                !blob.contains(token),
                "{label} leaked pair token plaintext: {blob}"
            );
        }
        for (label, v) in [("WorkerHost", &raw), ("as_public", &public)] {
            for key in [
                "pair_token",
                "pair_token_hash",
                "token_hash",
                "token",
                "token_plaintext",
            ] {
                assert!(v.get(key).is_none(), "{label} exposed {key}: {v}");
            }
        }
    }
}

async fn assert_repo_reads_omit_pair_plaintext(
    pool: &fleet_db::PgPool,
    workspace_id: Uuid,
    host_id: Uuid,
    tokens: &[&str],
) {
    let got = worker_hosts::get(pool, host_id)
        .await
        .expect("get")
        .expect("host exists");
    assert_eq!(got.id, host_id);
    assert_repo_host_omits_pair_plaintext(&got, tokens);

    let scoped = worker_hosts::get_in_workspace(pool, workspace_id, host_id)
        .await
        .expect("get_in_workspace")
        .expect("host in workspace");
    assert_repo_host_omits_pair_plaintext(&scoped, tokens);

    let listed = worker_hosts::list_for_workspace(pool, workspace_id)
        .await
        .expect("list_for_workspace");
    let listed_host = listed
        .iter()
        .find(|h| h.id == host_id)
        .expect("host in list");
    assert_repo_host_omits_pair_plaintext(listed_host, tokens);
    let list_json = serde_json::to_string(&listed).expect("list json");
    let list_dbg = format!("{listed:?}");
    for token in tokens {
        assert!(
            !list_json.contains(token),
            "list_for_workspace leaked pair token plaintext: {list_json}"
        );
        assert!(
            !list_dbg.contains(token),
            "list Debug leaked pair token plaintext"
        );
    }
}

fn assert_pair_conflict(err: DbError, needle: &str) {
    match err {
        DbError::Conflict(msg) => {
            assert!(
                msg.contains(needle),
                "expected conflict mentioning {needle:?}, got {msg}"
            );
        }
        other => panic!("expected Conflict ({needle}), got {other:?}"),
    }
}

/// Kind/compose bootstrap: `created_by IS NULL`, labels already `["cluster"]`.
async fn create_bootstrap_cluster_host(
    pool: &fleet_db::PgPool,
    workspace_id: Uuid,
    name: &str,
    expires_at: chrono::DateTime<Utc>,
    advertised_host: bool,
    advertised_container: bool,
) -> worker_hosts::PairedWorkerHost {
    let token = generate_pair_token();
    let mut new = NewWorkerHost::new(Uuid::now_v7(), workspace_id, name.to_string(), None);
    new.labels = vec!["cluster".into()];
    new.allow_host = advertised_host;
    new.allow_container = advertised_container;
    worker_hosts::create_pending(pool, &new, &hash_token(&token), expires_at)
        .await
        .expect("create_pending bootstrap");
    worker_hosts::consume_pair_token(
        pool,
        &token,
        name,
        "linux-x86_64",
        advertised_host,
        advertised_container,
        None,
    )
    .await
    .expect("pair bootstrap")
}

/// Re-run a data-migration UPDATE against only these rows (shared DATABASE_URL).
async fn apply_label_migration_to_hosts(pool: &fleet_db::PgPool, raw_sql: &str, host_ids: &[Uuid]) {
    let body = raw_sql.trim().trim_end_matches(';').trim();
    let sql = format!("{body}\n  AND id = ANY($1::uuid[])");
    sqlx::query(&sql)
        .bind(host_ids)
        .execute(pool)
        .await
        .unwrap_or_else(|e| panic!("apply migration: {e}\n{sql}"));
}

/// Heartbeat that only varies `machine_id` / `agent_runtimes` (the COALESCE pair).
async fn heartbeat_with_runtimes(
    pool: &fleet_db::PgPool,
    host_id: Uuid,
    machine_id: Option<Uuid>,
    agent_runtimes: Option<serde_json::Value>,
) -> fleet_db::models::WorkerHost {
    worker_hosts::heartbeat_with_machine(
        pool,
        host_id,
        true,
        false,
        None,
        None,
        None,
        0,
        None,
        None,
        None,
        None,
        None,
        machine_id,
        agent_runtimes,
    )
    .await
    .expect("heartbeat_with_machine")
}

/// Heartbeat that only varies `hardware` (the COALESCE snapshot).
async fn heartbeat_with_hardware(
    pool: &fleet_db::PgPool,
    host_id: Uuid,
    hardware: Option<serde_json::Value>,
) -> fleet_db::models::WorkerHost {
    worker_hosts::heartbeat(
        pool, host_id, true, false, None, None, None, 0, None, None, None, None, hardware,
    )
    .await
    .expect("heartbeat hardware")
}

fn public_gpu_names(host: &fleet_db::models::WorkerHost) -> Vec<String> {
    host.as_public(Utc::now())
        .hardware
        .as_ref()
        .and_then(|h| h.gpus.as_ref())
        .map(|gpus| {
            gpus.iter()
                .filter_map(|g| g.name.clone().filter(|n| !n.trim().is_empty()))
                .collect()
        })
        .unwrap_or_default()
}

fn persisted_gpu_entries(host: &fleet_db::models::WorkerHost) -> Vec<serde_json::Value> {
    host.hardware
        .as_ref()
        .and_then(|v| v.get("gpus"))
        .and_then(|g| g.as_array())
        .cloned()
        .unwrap_or_default()
}

fn gpu_entry_name(entry: &serde_json::Value) -> Option<&str> {
    match entry.get("name") {
        Some(serde_json::Value::Null) | None => None,
        Some(v) => v.as_str(),
    }
}

#[test]
fn pair_token_has_required_prefix_and_entropy() {
    let token = generate_pair_token();
    assert!(token.starts_with(PAIR_TOKEN_PREFIX), "{token}");
    assert_eq!(token.len(), PAIR_TOKEN_PREFIX.len() + 64);
    let hex = &token[PAIR_TOKEN_PREFIX.len()..];
    assert!(
        hex.chars().all(|c| c.is_ascii_hexdigit()),
        "expected hex after prefix: {token}"
    );
}

#[tokio::test]
async fn create_pending_then_list_for_workspace() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let other_ws = seed_user_with_workspace(&pool).await.1;
    let expires = Utc::now() + Duration::hours(1);

    let (a, _) = create_pending_with_token(&pool, workspace_id, user_id, "studio", expires).await;
    let (b, _) = create_pending_with_token(&pool, workspace_id, user_id, "lab", expires).await;
    let (_c, _) = create_pending_with_token(&pool, other_ws, user_id, "other", expires).await;

    let listed = worker_hosts::list_for_workspace(&pool, workspace_id)
        .await
        .expect("list");
    let ids: Vec<Uuid> = listed.iter().map(|h| h.id).collect();
    assert!(ids.contains(&a.id), "missing studio host");
    assert!(ids.contains(&b.id), "missing lab host");
    assert_eq!(listed.iter().filter(|h| h.id == a.id).count(), 1);
    assert!(listed.iter().all(|h| h.workspace_id == workspace_id));
    assert!(listed.iter().all(|h| h.status == "pending"));
    assert!(
        listed
            .iter()
            .all(|h| h.isolation == "native" && h.is_native_isolation()),
        "existing and new hosts default to native isolation"
    );
    assert!(listed.iter().all(|h| h.machine_token_hash.is_none()));
    assert!(listed.iter().all(|h| h.current_jobs == 0));
    assert!(listed.iter().all(|h| h.advertised_max_concurrent.is_none()));

    let fetched = worker_hosts::get(&pool, a.id)
        .await
        .expect("get")
        .expect("studio exists");
    assert_eq!(fetched.name, "studio");
    assert!(fetched.enabled);
    assert!(!fetched.paused);

    let scoped = worker_hosts::get_in_workspace(&pool, workspace_id, a.id)
        .await
        .expect("get_in_workspace");
    assert!(scoped.is_some());
    let cross = worker_hosts::get_in_workspace(&pool, other_ws, a.id)
        .await
        .expect("cross-workspace get");
    assert!(cross.is_none());
}

#[tokio::test]
async fn pair_token_works_once_second_consume_fails() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "once", expires).await;
    assert_pair_token_hashed_at_rest(&pool, host.id, &token).await;
    let (_, unused) = pair_token_hash_and_used_at(&pool, host.id, &token).await;
    assert!(unused.is_none(), "fresh pair token must be unused");

    let paired = pair_host(&pool, &token, "box-1").await;
    assert_eq!(paired.host.id, host.id);
    assert_eq!(paired.host.status, "online");
    assert_eq!(paired.host.hostname.as_deref(), Some("box-1"));
    assert_eq!(paired.host.platform.as_deref(), Some("linux-x86_64"));
    assert!(paired.host.advertised_host);
    assert!(!paired.host.advertised_container);
    assert!(paired.host.paired_at.is_some());
    assert!(paired.host.last_seen_at.is_some());
    assert!(paired.host.machine_token_hash.is_some());
    assert_eq!(
        paired.host.machine_id,
        Some(host.id),
        "first pair assigns a durable machine id"
    );
    assert!(paired
        .machine_token
        .starts_with(worker_hosts::MACHINE_TOKEN_PREFIX));
    let (_, used) = pair_token_hash_and_used_at(&pool, host.id, &token).await;
    assert!(used.is_some(), "successful consume must set used_at");
    assert_pair_token_hashed_at_rest(&pool, host.id, &token).await;

    let authed = worker_hosts::authenticate_machine(&pool, &paired.machine_token)
        .await
        .expect("auth")
        .expect("machine token must authenticate");
    assert_eq!(authed.id, host.id);

    let err =
        worker_hosts::consume_pair_token(&pool, &token, "box-1", "linux-x86_64", true, false, None)
            .await
            .expect_err("second consume must fail");
    // Consume-once is used_at, not "host is no longer pending".
    assert_pair_conflict(err, "used");

    let still = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host remains");
    assert_eq!(still.status, "online");
    assert_eq!(still.machine_token_hash, paired.host.machine_token_hash);
    let (_, still_used) = pair_token_hash_and_used_at(&pool, host.id, &token).await;
    assert_eq!(still_used, used, "second consume must not rewrite used_at");
}

/// Catalog: pair token TTL is one hour (HTTP `PAIR_TTL`) and single-use.
/// `create_pending` stores the `now + PAIR_TTL` window HTTP issues. A token
/// still inside that hour pairs once; reuse and an aged-past-1h token fail.
/// Sibling `pair_token_consume_once_and_expired_must_fail` owns used_at /
/// leftover / race / `expires_at == now` — this locks the 1h duration.
#[tokio::test]
async fn pair_token_one_hour_ttl_cannot_be_reused() {
    assert_eq!(
        PAIR_TTL,
        Duration::hours(1),
        "catalog pair token TTL must be one hour"
    );
    assert_eq!(
        PAIR_TTL.num_seconds(),
        3600,
        "PAIR_TTL must be exactly 3600 seconds"
    );

    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let issued_at = Utc::now();
    let expires = issued_at + PAIR_TTL;
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "ttl-1h", expires).await;

    let (stored_expires, stored_created): (chrono::DateTime<Utc>, chrono::DateTime<Utc>) =
        sqlx::query_as(
            r#"
            SELECT expires_at, created_at
            FROM worker_host_pair_tokens
            WHERE host_id = $1 AND token_hash = $2
            "#,
        )
        .bind(host.id)
        .bind(hash_token(&token))
        .fetch_one(&pool)
        .await
        .expect("stored pair token window");
    let window = stored_expires - stored_created;
    assert!(
        window >= Duration::minutes(59) && window <= Duration::minutes(61),
        "stored pair token window must be 1 hour, got {window}"
    );
    let issued_skew = (stored_expires - expires).num_milliseconds().abs();
    assert!(
        issued_skew <= 2_000,
        "stored expires_at must be the issued now+1h ({expires}), got {stored_expires}"
    );

    let paired = worker_hosts::consume_pair_token(
        &pool,
        &token,
        "box-ttl",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect("token inside the 1h window must pair");
    assert_eq!(paired.host.id, host.id);
    assert_eq!(paired.host.status, "online");
    let (_, used_at) = pair_token_hash_and_used_at(&pool, host.id, &token).await;
    assert!(used_at.is_some(), "first consume must set used_at");

    let reuse = worker_hosts::consume_pair_token(
        &pool,
        &token,
        "box-ttl",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect_err("pair token cannot be reused");
    assert_pair_conflict(reuse, "used");
    let after_reuse = worker_hosts::get(&pool, host.id)
        .await
        .expect("get ttl host")
        .expect("ttl host remains");
    assert_eq!(after_reuse.status, "online");
    assert_eq!(
        after_reuse.machine_token_hash, paired.host.machine_token_hash,
        "failed reuse must not rotate the machine token"
    );
    let (_, still_used) = pair_token_hash_and_used_at(&pool, host.id, &token).await;
    assert_eq!(still_used, used_at, "failed reuse must not rewrite used_at");

    // Guest after the catalog hour: issue with PAIR_TTL, then elapse that hour.
    let (aged, aged_token) = create_pending_with_token(
        &pool,
        workspace_id,
        user_id,
        "ttl-aged",
        Utc::now() + PAIR_TTL,
    )
    .await;
    let aged_expires: chrono::DateTime<Utc> =
        sqlx::query_scalar("SELECT expires_at FROM worker_host_pair_tokens WHERE host_id = $1")
            .bind(aged.id)
            .fetch_one(&pool)
            .await
            .expect("aged expires_at");
    sqlx::query("UPDATE worker_host_pair_tokens SET expires_at = $2 WHERE host_id = $1")
        .bind(aged.id)
        .bind(aged_expires - PAIR_TTL - Duration::seconds(1))
        .execute(&pool)
        .await
        .expect("elapse the catalog 1h window");
    let aged_err = worker_hosts::consume_pair_token(
        &pool,
        &aged_token,
        "box-aged-1h",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect_err("token past the 1h TTL must fail");
    assert_pair_conflict(aged_err, "expired");
    let still_aged = worker_hosts::get(&pool, aged.id)
        .await
        .expect("get aged")
        .expect("aged remains");
    assert_eq!(still_aged.status, "pending");
    assert!(still_aged.machine_token_hash.is_none());
    let (_, aged_used) = pair_token_hash_and_used_at(&pool, aged.id, &aged_token).await;
    assert!(aged_used.is_none(), "expired 1h token must not set used_at");

    // Control: an unused token still inside the 1h window still pairs.
    let (live, live_token) = create_pending_with_token(
        &pool,
        workspace_id,
        user_id,
        "ttl-live",
        Utc::now() + PAIR_TTL,
    )
    .await;
    let live_paired = pair_host(&pool, &live_token, "box-ttl-live").await;
    assert_eq!(live_paired.host.id, live.id);
    assert_eq!(live_paired.host.status, "online");
}

/// Catalog: pair token is single-use and expires. Second consume and an
/// expired token fail; only the hash is stored. used_at is the consume-once
/// gate — resetting the host to pending must not revive a used or leftover token.
#[tokio::test]
async fn pair_token_consume_once_and_expired_must_fail() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);

    let (once, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "consume-once", expires).await;
    assert_pair_token_hashed_at_rest(&pool, once.id, &token).await;

    // A second unused token on the same host must die when the first is consumed.
    let leftover = generate_pair_token();
    sqlx::query(
        r#"
        INSERT INTO worker_host_pair_tokens (
            id, host_id, workspace_id, token_hash, expires_at
        )
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(once.id)
    .bind(workspace_id)
    .bind(hash_token(&leftover))
    .bind(expires)
    .execute(&pool)
    .await
    .expect("insert leftover unused pair token");
    let (_, leftover_unused) = pair_token_hash_and_used_at(&pool, once.id, &leftover).await;
    assert!(leftover_unused.is_none());

    let paired = worker_hosts::consume_pair_token(
        &pool,
        &token,
        "box-once",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect("first consume");
    assert_eq!(paired.host.id, once.id);
    assert_eq!(paired.host.status, "online");
    let machine_hash = paired
        .host
        .machine_token_hash
        .clone()
        .expect("machine token hash after pair");
    let (_, used_at) = pair_token_hash_and_used_at(&pool, once.id, &token).await;
    assert!(used_at.is_some(), "first consume must set used_at");
    let (_, leftover_used) = pair_token_hash_and_used_at(&pool, once.id, &leftover).await;
    assert!(
        leftover_used.is_some(),
        "leftover unused tokens must be marked used when the host leaves pending"
    );
    assert_pair_token_hashed_at_rest(&pool, once.id, &token).await;
    assert_pair_token_hashed_at_rest(&pool, once.id, &leftover).await;

    let reuse = worker_hosts::consume_pair_token(
        &pool,
        &token,
        "box-once",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect_err("second consume of the same pair token must fail");
    assert_pair_conflict(reuse, "used");

    // used_at is the lock, not host.status. A product that only checked
    // "still pending" would re-pair after this reset.
    sqlx::query("UPDATE worker_hosts SET status = 'pending' WHERE id = $1")
        .bind(once.id)
        .execute(&pool)
        .await
        .expect("reset host to pending");
    let reuse_pending = worker_hosts::consume_pair_token(
        &pool,
        &token,
        "box-once-again",
        "linux-aarch64",
        true,
        true,
        None,
    )
    .await
    .expect_err("used pair token must stay dead after status reset");
    assert_pair_conflict(reuse_pending, "used");

    let leftover_err = worker_hosts::consume_pair_token(
        &pool,
        &leftover,
        "box-leftover",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect_err("leftover pair token must stay dead after the first consume");
    assert_pair_conflict(leftover_err, "used");

    let after_reuse = worker_hosts::get(&pool, once.id)
        .await
        .expect("get once")
        .expect("once remains");
    assert_eq!(
        after_reuse.status, "pending",
        "failed reuse must not promote the host"
    );
    assert_eq!(
        after_reuse.machine_token_hash.as_deref(),
        Some(machine_hash.as_str()),
        "failed reuse must not rotate the machine token"
    );
    assert_eq!(after_reuse.hostname.as_deref(), Some("box-once"));
    assert_eq!(after_reuse.platform.as_deref(), Some("linux-x86_64"));
    assert!(
        worker_hosts::authenticate_machine(&pool, &paired.machine_token)
            .await
            .expect("auth after failed reuse")
            .is_some(),
        "original machine token must still authenticate"
    );
    let (_, used_again) = pair_token_hash_and_used_at(&pool, once.id, &token).await;
    assert_eq!(used_again, used_at, "failed reuse must not rewrite used_at");

    // Already-used row on a host that never left pending.
    let (preused, preused_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "preused", expires).await;
    sqlx::query("UPDATE worker_host_pair_tokens SET used_at = now() WHERE host_id = $1")
        .bind(preused.id)
        .execute(&pool)
        .await
        .expect("mark used while still pending");
    let preused_err = worker_hosts::consume_pair_token(
        &pool,
        &preused_token,
        "box-preused",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect_err("used_at must block consume even while host is pending");
    assert_pair_conflict(preused_err, "used");
    let still_preused = worker_hosts::get(&pool, preused.id)
        .await
        .expect("get preused")
        .expect("preused remains");
    assert_eq!(still_preused.status, "pending");
    assert!(still_preused.machine_token_hash.is_none());
    assert!(still_preused.paired_at.is_none());
    assert_pair_token_hashed_at_rest(&pool, preused.id, &preused_token).await;

    // Expired at insert: fail, do not consume, host stays pending.
    let expired_at = Utc::now() - Duration::seconds(1);
    let (stale, stale_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "stale", expired_at).await;
    assert_pair_token_hashed_at_rest(&pool, stale.id, &stale_token).await;
    let stale_err = worker_hosts::consume_pair_token(
        &pool,
        &stale_token,
        "box-stale",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect_err("expired pair token must fail");
    assert_pair_conflict(stale_err, "expired");
    let still_stale = worker_hosts::get(&pool, stale.id)
        .await
        .expect("get stale")
        .expect("stale remains");
    assert_eq!(still_stale.status, "pending");
    assert!(still_stale.machine_token_hash.is_none());
    assert!(still_stale.paired_at.is_none());
    let (_, stale_used) = pair_token_hash_and_used_at(&pool, stale.id, &stale_token).await;
    assert!(stale_used.is_none(), "expired consume must not set used_at");
    assert_pair_token_hashed_at_rest(&pool, stale.id, &stale_token).await;

    // Catalog TTL is 1 hour: a still-valid +1h token that is then aged past
    // expires_at must fail the same way (consume reads the row, not a window).
    let (aged, aged_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "aged", expires).await;
    sqlx::query(
        "UPDATE worker_host_pair_tokens SET expires_at = now() - interval '1 second' WHERE host_id = $1",
    )
    .bind(aged.id)
    .execute(&pool)
    .await
    .expect("age pair token past expiry");
    let aged_err = worker_hosts::consume_pair_token(
        &pool,
        &aged_token,
        "box-aged",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect_err("aged pair token must fail");
    assert_pair_conflict(aged_err, "expired");
    let still_aged = worker_hosts::get(&pool, aged.id)
        .await
        .expect("get aged")
        .expect("aged remains");
    assert_eq!(still_aged.status, "pending");
    assert!(still_aged.machine_token_hash.is_none());
    let (_, aged_used) = pair_token_hash_and_used_at(&pool, aged.id, &aged_token).await;
    assert!(aged_used.is_none(), "aged consume must not set used_at");

    // Consume reads expires_at, not created_at + 1h. An old row whose
    // expires_at is still in the future must still pair.
    let (window, window_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "window", expires).await;
    sqlx::query(
        "UPDATE worker_host_pair_tokens SET created_at = now() - interval '2 hours' WHERE host_id = $1",
    )
    .bind(window.id)
    .execute(&pool)
    .await
    .expect("age created_at without touching expires_at");
    let window_paired = worker_hosts::consume_pair_token(
        &pool,
        &window_token,
        "box-window",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect("future expires_at must consume even if created_at is 2h old");
    assert_eq!(window_paired.host.id, window.id);
    assert_eq!(window_paired.host.status, "online");
    assert_pair_token_hashed_at_rest(&pool, window.id, &window_token).await;

    // expires_at == now is already expired (consume uses <=).
    let (boundary, boundary_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "boundary", Utc::now()).await;
    let boundary_err = worker_hosts::consume_pair_token(
        &pool,
        &boundary_token,
        "box-boundary",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect_err("expires_at == now must fail as expired");
    assert_pair_conflict(boundary_err, "expired");
    let still_boundary = worker_hosts::get(&pool, boundary.id)
        .await
        .expect("get boundary")
        .expect("boundary remains");
    assert_eq!(still_boundary.status, "pending");
    assert!(still_boundary.machine_token_hash.is_none());
    let (_, boundary_used) = pair_token_hash_and_used_at(&pool, boundary.id, &boundary_token).await;
    assert!(
        boundary_used.is_none(),
        "expires_at == now must not set used_at"
    );

    // Parallel consume-once: FOR UPDATE admits exactly one winner.
    let (race, race_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "race", expires).await;
    let (first, second) = tokio::join!(
        worker_hosts::consume_pair_token(
            &pool,
            &race_token,
            "box-race-a",
            "linux-x86_64",
            true,
            false,
            None,
        ),
        worker_hosts::consume_pair_token(
            &pool,
            &race_token,
            "box-race-b",
            "linux-x86_64",
            true,
            false,
            None,
        ),
    );
    let wins = [&first, &second].iter().filter(|r| r.is_ok()).count();
    assert_eq!(
        wins, 1,
        "exactly one parallel consume must succeed: {first:?} {second:?}"
    );
    match first
        .as_ref()
        .err()
        .or(second.as_ref().err())
        .expect("loser")
    {
        DbError::Conflict(msg) => {
            assert!(
                msg.contains("used"),
                "expected conflict mentioning \"used\", got {msg}"
            );
        }
        other => panic!("expected Conflict (used), got {other:?}"),
    }
    let raced = worker_hosts::get(&pool, race.id)
        .await
        .expect("get race")
        .expect("race remains");
    assert_eq!(raced.status, "online");
    let (_, race_used) = pair_token_hash_and_used_at(&pool, race.id, &race_token).await;
    assert!(race_used.is_some(), "winning consume must set used_at");
    assert_pair_token_hashed_at_rest(&pool, race.id, &race_token).await;
    let winner_token = first
        .as_ref()
        .ok()
        .or(second.as_ref().ok())
        .expect("winner")
        .machine_token
        .clone();
    assert!(
        worker_hosts::authenticate_machine(&pool, &winner_token)
            .await
            .expect("auth race winner")
            .is_some(),
        "winning machine token must authenticate"
    );

    // Control: another unused, unexpired token still pairs so this is not a
    // tautology that every consume_pair_token fails.
    let (live, live_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "live", expires).await;
    let live_paired = worker_hosts::consume_pair_token(
        &pool,
        &live_token,
        "box-live",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect("unexpired unused pair token must still consume");
    assert_eq!(live_paired.host.id, live.id);
    assert_eq!(live_paired.host.status, "online");
    let (_, live_used) = pair_token_hash_and_used_at(&pool, live.id, &live_token).await;
    assert!(live_used.is_some());
    assert_pair_token_hashed_at_rest(&pool, live.id, &live_token).await;
}

#[tokio::test]
async fn re_pair_same_machine_id_reclaims_existing_row() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let mid = Uuid::now_v7();

    let (first, token_a) =
        create_pending_with_token(&pool, workspace_id, user_id, "box", expires).await;
    let paired = worker_hosts::consume_pair_token(
        &pool,
        &token_a,
        "box-old-hostname",
        "linux-x86_64",
        true,
        false,
        Some(mid),
    )
    .await
    .expect("first pair");
    assert_eq!(paired.host.id, first.id);
    assert_eq!(paired.host.machine_id, Some(mid));
    let old_token = paired.machine_token.clone();

    // Operator-owned CP fields must survive remove + re-pair of the same box.
    let patched = worker_hosts::patch_cp_settings(
        &pool,
        first.id,
        &HostCpSettings {
            name: Some("studio-kept".into()),
            labels: Some(vec!["gpu".into()]),
            max_concurrent: Some(7),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("patch cp settings");
    assert_eq!(patched.settings_gen, 1);

    worker_hosts::heartbeat(
        &pool,
        first.id,
        true,
        false,
        None,
        None,
        Some("disk full"),
        0,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("heartbeat last_error");

    let revoked = worker_hosts::revoke(&pool, first.id).await.expect("revoke");
    assert_eq!(revoked.status, "disabled");
    assert!(revoked.machine_token_hash.is_none());
    assert_eq!(
        revoked.machine_id,
        Some(mid),
        "revoke must keep machine.id so the same box can reclaim the row"
    );
    assert_eq!(revoked.name, "studio-kept");
    assert_eq!(revoked.labels, vec!["gpu".to_string()]);
    assert_eq!(revoked.max_concurrent, 7);
    assert!(
        worker_hosts::authenticate_machine(&pool, &old_token)
            .await
            .expect("auth after revoke")
            .is_none(),
        "revoked machine token must not authenticate"
    );
    let by_mid = worker_hosts::find_by_machine_id(&pool, workspace_id, mid)
        .await
        .expect("find_by_machine_id after revoke")
        .expect("revoked row still keyed by machine.id");
    assert_eq!(by_mid.id, first.id);

    // New pending (operator re-add). Same durable box id, different hostname —
    // machine.id is the identity; hostname+platform is only a fallback when omitted.
    let (second, token_b) =
        create_pending_with_token(&pool, workspace_id, user_id, "throwaway", expires).await;
    let again = worker_hosts::consume_pair_token(
        &pool,
        &token_b,
        "box-renamed-os",
        "linux-aarch64",
        true,
        true,
        Some(mid),
    )
    .await
    .expect("re-pair");

    assert_eq!(again.host.id, first.id, "must revive the original row");
    assert_ne!(again.host.id, second.id, "must not promote the new pending");
    assert_eq!(again.host.status, "online");
    assert!(again.host.enabled);
    assert_eq!(again.host.machine_id, Some(mid));
    assert_eq!(
        again.host.name, "studio-kept",
        "operator name must survive revoke+re-pair"
    );
    assert_eq!(again.host.labels, vec!["gpu".to_string()]);
    assert_eq!(again.host.max_concurrent, 7);
    assert_eq!(
        again.host.settings_gen, 1,
        "settings_gen must not reset on reclaim"
    );
    assert_eq!(again.host.hostname.as_deref(), Some("box-renamed-os"));
    assert_eq!(again.host.platform.as_deref(), Some("linux-aarch64"));
    assert!(again.host.advertised_host);
    assert!(again.host.advertised_container);
    assert!(
        again.host.last_error.is_none(),
        "reclaim clears last_error from the prior life"
    );
    assert!(again.host.paired_at.is_some());
    assert_ne!(
        again.machine_token, old_token,
        "re-pair must issue a new machine token"
    );
    assert!(again
        .machine_token
        .starts_with(worker_hosts::MACHINE_TOKEN_PREFIX));

    let authed = worker_hosts::authenticate_machine(&pool, &again.machine_token)
        .await
        .expect("auth new token")
        .expect("new machine token must authenticate");
    assert_eq!(authed.id, first.id);
    assert!(
        worker_hosts::authenticate_machine(&pool, &old_token)
            .await
            .expect("auth old token after reclaim")
            .is_none(),
        "old machine token must stay dead after reclaim"
    );

    let leftover = worker_hosts::get(&pool, second.id)
        .await
        .expect("get leftover")
        .expect("pending leftover exists");
    assert_eq!(leftover.status, "disabled");
    assert!(
        !leftover.enabled,
        "discarded pending clone must not stay enabled"
    );
    assert!(
        leftover.machine_id.is_none(),
        "leftover must not steal the durable machine.id"
    );
    assert!(leftover.machine_token_hash.is_none());

    let found = worker_hosts::find_by_machine_id(&pool, workspace_id, mid)
        .await
        .expect("find_by_machine_id after reclaim")
        .expect("machine.id maps to one row");
    assert_eq!(found.id, first.id);

    let reuse_err = worker_hosts::consume_pair_token(
        &pool,
        &token_b,
        "box-renamed-os",
        "linux-aarch64",
        true,
        true,
        Some(mid),
    )
    .await
    .expect_err("leftover pair token must be consumed");
    match reuse_err {
        DbError::Conflict(msg) => assert!(
            msg.contains("used") || msg.contains("pending"),
            "unexpected conflict: {msg}"
        ),
        other => panic!("expected Conflict on leftover token reuse, got {other:?}"),
    }

    let assignable = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
        .await
        .expect("list_assignable");
    let ids: Vec<Uuid> = assignable.iter().map(|h| h.id).collect();
    assert!(
        ids.contains(&first.id),
        "reclaimed row must be assignable: {ids:?}"
    );
    assert!(
        !ids.contains(&second.id),
        "leftover clone must not be assignable: {ids:?}"
    );

    // Durable across a second remove + re-pair on yet another pending host.
    worker_hosts::revoke(&pool, first.id)
        .await
        .expect("second revoke");
    let (third, token_c) =
        create_pending_with_token(&pool, workspace_id, user_id, "throwaway-2", expires).await;
    let third_pair = worker_hosts::consume_pair_token(
        &pool,
        &token_c,
        "box-third",
        "linux-x86_64",
        true,
        false,
        Some(mid),
    )
    .await
    .expect("second re-pair");
    assert_eq!(
        third_pair.host.id, first.id,
        "same machine.id must keep reclaiming the original row"
    );
    let third_leftover = worker_hosts::get(&pool, third.id)
        .await
        .expect("get third leftover")
        .expect("third leftover exists");
    assert_eq!(third_leftover.status, "disabled");
    assert!(!third_leftover.enabled);
}

/// machine.id is the durable key: after revoke, a new pending that sends that id
/// must revive the revoked row even when its hostname matches a different live host.
#[tokio::test]
async fn re_pair_same_machine_id_reclaims_not_hostname_collision() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let mid_a = Uuid::now_v7();
    let mid_b = Uuid::now_v7();

    let (host_a, token_a) =
        create_pending_with_token(&pool, workspace_id, user_id, "alpha", expires).await;
    let a = worker_hosts::consume_pair_token(
        &pool,
        &token_a,
        "alpha-box",
        "linux-x86_64",
        true,
        false,
        Some(mid_a),
    )
    .await
    .expect("pair a");
    assert_eq!(a.host.id, host_a.id);

    let (host_b, token_b) =
        create_pending_with_token(&pool, workspace_id, user_id, "bravo", expires).await;
    let b = worker_hosts::consume_pair_token(
        &pool,
        &token_b,
        "bravo-box",
        "linux-x86_64",
        true,
        false,
        Some(mid_b),
    )
    .await
    .expect("pair b");
    assert_eq!(b.host.id, host_b.id);

    worker_hosts::revoke(&pool, host_a.id)
        .await
        .expect("revoke a");

    let (pending, token_c) =
        create_pending_with_token(&pool, workspace_id, user_id, "clone", expires).await;
    let again = worker_hosts::consume_pair_token(
        &pool,
        &token_c,
        "bravo-box",
        "linux-x86_64",
        true,
        false,
        Some(mid_a),
    )
    .await
    .expect("re-pair a via colliding hostname");

    assert_eq!(
        again.host.id, host_a.id,
        "machine.id must reclaim the revoked row, not the live hostname match"
    );
    assert_eq!(again.host.machine_id, Some(mid_a));
    assert_eq!(again.host.hostname.as_deref(), Some("bravo-box"));
    assert_ne!(again.host.id, host_b.id);
    assert_ne!(again.host.id, pending.id);

    let still_b = worker_hosts::get(&pool, host_b.id)
        .await
        .expect("get b")
        .expect("b exists");
    assert_eq!(still_b.status, "online");
    assert_eq!(still_b.machine_id, Some(mid_b));
    assert_eq!(still_b.hostname.as_deref(), Some("bravo-box"));

    let leftover = worker_hosts::get(&pool, pending.id)
        .await
        .expect("get leftover")
        .expect("leftover exists");
    assert_eq!(leftover.status, "disabled");
    assert!(!leftover.enabled);
}

/// Same physical box id in another workspace is a new row, not a steal.
#[tokio::test]
async fn re_pair_same_machine_id_does_not_reclaim_other_workspace() {
    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (user_b, ws_b) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let mid = Uuid::now_v7();

    let (host_a, token_a) = create_pending_with_token(&pool, ws_a, user_a, "a-box", expires).await;
    let paired_a = worker_hosts::consume_pair_token(
        &pool,
        &token_a,
        "shared-box",
        "linux-x86_64",
        true,
        false,
        Some(mid),
    )
    .await
    .expect("pair a");
    assert_eq!(paired_a.host.id, host_a.id);
    worker_hosts::revoke(&pool, host_a.id)
        .await
        .expect("revoke a");

    let (host_b, token_b) = create_pending_with_token(&pool, ws_b, user_b, "b-box", expires).await;
    let paired_b = worker_hosts::consume_pair_token(
        &pool,
        &token_b,
        "shared-box",
        "linux-x86_64",
        true,
        false,
        Some(mid),
    )
    .await
    .expect("pair b");
    assert_eq!(
        paired_b.host.id, host_b.id,
        "same machine.id in another workspace must not reclaim that workspace's row"
    );
    assert_eq!(paired_b.host.workspace_id, ws_b);
    assert_eq!(paired_b.host.machine_id, Some(mid));
    assert_eq!(paired_b.host.status, "online");

    let still_a = worker_hosts::get(&pool, host_a.id)
        .await
        .expect("get a")
        .expect("a exists");
    assert_eq!(still_a.status, "disabled");
    assert_eq!(still_a.workspace_id, ws_a);
    assert_eq!(still_a.machine_id, Some(mid));

    let found_a = worker_hosts::find_by_machine_id(&pool, ws_a, mid)
        .await
        .expect("find a")
        .expect("ws a still has the revoked row");
    assert_eq!(found_a.id, host_a.id);
    let found_b = worker_hosts::find_by_machine_id(&pool, ws_b, mid)
        .await
        .expect("find b")
        .expect("ws b has its own row");
    assert_eq!(found_b.id, host_b.id);
}

#[tokio::test]
async fn re_pair_same_hostname_without_machine_id_reclaims_existing_row() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);

    let (first, token_a) =
        create_pending_with_token(&pool, workspace_id, user_id, "box", expires).await;
    let paired = pair_host(&pool, &token_a, "studio-pc").await;
    assert_eq!(paired.host.id, first.id);
    assert_eq!(paired.host.machine_id, Some(first.id));

    worker_hosts::revoke(&pool, first.id).await.expect("remove");

    let (second, token_b) =
        create_pending_with_token(&pool, workspace_id, user_id, "box-again", expires).await;
    let again = pair_host(&pool, &token_b, "studio-pc").await;
    assert_eq!(again.host.id, first.id, "same hostname must revive the row");
    assert_eq!(again.host.status, "online");
    assert!(again.host.enabled);
    assert_eq!(again.host.machine_id, Some(first.id));

    let leftover = worker_hosts::get(&pool, second.id)
        .await
        .expect("get leftover")
        .expect("pending leftover exists");
    assert_eq!(leftover.status, "disabled");
}

#[tokio::test]
async fn re_pair_same_hostname_without_machine_id_does_not_steal_live_row() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);

    let (first, token_a) =
        create_pending_with_token(&pool, workspace_id, user_id, "box-a", expires).await;
    let paired = pair_host(&pool, &token_a, "ubuntu").await;
    assert_eq!(paired.host.id, first.id);
    let first_token = paired.machine_token.clone();

    let (second, token_b) =
        create_pending_with_token(&pool, workspace_id, user_id, "box-b", expires).await;
    let other = pair_host(&pool, &token_b, "ubuntu").await;
    assert_eq!(
        other.host.id, second.id,
        "live same-hostname pair must become a new row"
    );
    assert_ne!(other.host.id, first.id);
    assert_eq!(other.host.status, "online");

    let still = worker_hosts::get(&pool, first.id)
        .await
        .expect("get first")
        .expect("first still exists");
    assert_eq!(still.status, "online");
    assert_eq!(still.machine_token_hash, paired.host.machine_token_hash);

    let authed = worker_hosts::authenticate_machine(&pool, &first_token)
        .await
        .expect("auth")
        .expect("original token must still work");
    assert_eq!(authed.id, first.id);
}

#[tokio::test]
async fn re_pair_same_hostname_without_machine_id_reclaims_stale_row() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);

    let (first, token_a) =
        create_pending_with_token(&pool, workspace_id, user_id, "box", expires).await;
    let paired = pair_host(&pool, &token_a, "studio-pc").await;
    assert_eq!(paired.host.id, first.id);

    sqlx::query(
        "UPDATE worker_hosts SET last_seen_at = now() - interval '120 seconds' WHERE id = $1",
    )
    .bind(first.id)
    .execute(&pool)
    .await
    .expect("age last_seen");

    let (second, token_b) =
        create_pending_with_token(&pool, workspace_id, user_id, "box-again", expires).await;
    let again = pair_host(&pool, &token_b, "studio-pc").await;
    assert_eq!(
        again.host.id, first.id,
        "stale same-hostname pair must revive the row"
    );
    assert_eq!(again.host.status, "online");
    assert!(again.host.enabled);
    assert_ne!(
        again.machine_token, paired.machine_token,
        "reclaim rotates the token on the stale row"
    );

    let leftover = worker_hosts::get(&pool, second.id)
        .await
        .expect("get leftover")
        .expect("pending leftover exists");
    assert_eq!(leftover.status, "disabled");
}

#[tokio::test]
async fn two_machine_ids_same_hostname_stay_two_rows() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let mid_a = Uuid::now_v7();
    let mid_b = Uuid::now_v7();

    let (first, token_a) =
        create_pending_with_token(&pool, workspace_id, user_id, "box-a", expires).await;
    let a = worker_hosts::consume_pair_token(
        &pool,
        &token_a,
        "ubuntu",
        "linux-x86_64",
        true,
        false,
        Some(mid_a),
    )
    .await
    .expect("pair a");
    assert_eq!(a.host.id, first.id);
    assert_eq!(a.host.machine_id, Some(mid_a));

    let (second, token_b) =
        create_pending_with_token(&pool, workspace_id, user_id, "box-b", expires).await;
    let b = worker_hosts::consume_pair_token(
        &pool,
        &token_b,
        "ubuntu",
        "linux-x86_64",
        true,
        false,
        Some(mid_b),
    )
    .await
    .expect("pair b");
    assert_eq!(b.host.id, second.id, "distinct machine.id must not reclaim");
    assert_eq!(b.host.machine_id, Some(mid_b));
    assert_ne!(a.host.id, b.host.id);

    let still = worker_hosts::get(&pool, first.id)
        .await
        .expect("get first")
        .expect("first still exists");
    assert_eq!(still.status, "online");
    assert_eq!(still.machine_id, Some(mid_a));
}

#[tokio::test]
async fn patch_bumps_settings_gen_until_ack() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "ack", expires).await;
    pair_host(&pool, &token, "ack-box").await;

    let before = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(before.settings_gen, 0);
    assert!(!before.settings_pending());

    let patched = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            allow_host: Some(false),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("patch");
    assert_eq!(patched.settings_gen, 1);
    assert_eq!(patched.settings_ack_gen, 0);
    assert!(patched.settings_pending());

    let acked = worker_hosts::ack_settings(&pool, host.id, 1)
        .await
        .expect("ack");
    assert_eq!(acked.settings_ack_gen, 1);
    assert!(!acked.settings_pending());
}

/// catalog:unit-apply-ack — each Apply PATCH increments `settings_gen`.
#[tokio::test]
async fn each_settings_apply_increments_settings_gen() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("gen-bump");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let before = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(before.settings_gen, 0);
    assert_eq!(before.settings_ack_gen, 0);
    assert!(!before.settings_pending());

    let first = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            allow_host: Some(false),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("first apply");
    assert_eq!(first.settings_gen, 1, "first apply must bump 0 → 1");
    assert_eq!(first.settings_ack_gen, 0);
    assert!(first.settings_pending());

    let second = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            paused: Some(true),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("second apply");
    assert_eq!(second.settings_gen, 2, "second apply must bump 1 → 2");
    assert_eq!(second.settings_ack_gen, 0);
    assert!(second.settings_pending());

    let third = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            max_concurrent: Some(2),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("third apply");
    assert_eq!(third.settings_gen, 3, "third apply must bump 2 → 3");
    assert_eq!(third.settings_ack_gen, 0);
    assert!(third.settings_pending());

    let noop = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            paused: Some(true),
            max_concurrent: Some(2),
            allow_host: Some(false),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("identical apply");
    assert_eq!(
        noop.settings_gen, 3,
        "no-op PATCH must not increment settings_gen"
    );
    assert_eq!(noop.settings_ack_gen, 0);
    assert!(noop.settings_pending());

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(loaded.settings_gen, 3);
    assert_eq!(loaded.settings_ack_gen, 0);
    assert!(loaded.settings_pending());
}

/// catalog:unit-apply-ack — heartbeat ack of an older gen must not clear pending.
#[tokio::test]
async fn ack_behind_settings_gen_stays_pending() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("gen-behind");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            allow_host: Some(false),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("apply gen 1");
    let latest = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            paused: Some(true),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("apply gen 2");
    assert_eq!(latest.settings_gen, 2);
    assert_eq!(latest.settings_ack_gen, 0);
    assert!(latest.settings_pending());

    let err = worker_hosts::ack_settings(&pool, host.id, 1)
        .await
        .expect_err("ack of gen 1 while desired is 2 must fail");
    match err {
        DbError::NotFound(msg) => {
            assert!(
                msg.contains("not found"),
                "stale ack currently returns NotFound: {msg}"
            );
        }
        other => panic!("expected NotFound on behind ack, got {other:?}"),
    }

    let after_stale = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(
        after_stale.settings_gen, 2,
        "behind ack must not change settings_gen"
    );
    assert_eq!(
        after_stale.settings_ack_gen, 0,
        "behind ack must not advance settings_ack_gen"
    );
    assert!(
        after_stale.settings_pending(),
        "ack of an old gen must leave apply pending"
    );

    let err_zero = worker_hosts::ack_settings(&pool, host.id, 0)
        .await
        .expect_err("ack of gen 0 after apply must fail");
    match err_zero {
        DbError::NotFound(_) => {}
        other => panic!("expected NotFound on gen-0 ack, got {other:?}"),
    }
    let err_ahead = worker_hosts::ack_settings(&pool, host.id, 99)
        .await
        .expect_err("ack of a future gen must fail");
    match err_ahead {
        DbError::NotFound(_) => {}
        other => panic!("expected NotFound on future ack, got {other:?}"),
    }

    let still = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(still.settings_gen, 2);
    assert_eq!(still.settings_ack_gen, 0);
    assert!(still.settings_pending());

    let current = worker_hosts::ack_settings(&pool, host.id, 2)
        .await
        .expect("ack of current gen must succeed after stale attempts");
    assert_eq!(current.settings_ack_gen, 2);
    assert_eq!(current.settings_gen, 2);
    assert!(!current.settings_pending());

    let reapplied = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            drain: Some(true),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("re-apply after ack");
    assert_eq!(reapplied.settings_gen, 3);
    assert_eq!(reapplied.settings_ack_gen, 2);
    assert!(reapplied.settings_pending());

    let err_prior = worker_hosts::ack_settings(&pool, host.id, 2)
        .await
        .expect_err("re-acking the previous gen after a new apply must fail");
    match err_prior {
        DbError::NotFound(_) => {}
        other => panic!("expected NotFound on prior-gen ack, got {other:?}"),
    }
    let after_prior = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(after_prior.settings_gen, 3);
    assert_eq!(
        after_prior.settings_ack_gen, 2,
        "prior-gen ack must not rewind or jump settings_ack_gen"
    );
    assert!(after_prior.settings_pending());
}

/// Heartbeat itself must not ack; echoing an old gen after heartbeat stays pending.
#[tokio::test]
async fn heartbeat_ack_old_settings_gen_stays_pending() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("gen-hb-ack");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let first = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            allow_container: Some(false),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("first apply");
    assert_eq!(first.settings_gen, 1);
    assert!(first.settings_pending());

    let beat = worker_hosts::heartbeat(
        &pool,
        host.id,
        true,
        false,
        Some("linux-x86_64"),
        Some(&format!("{name}-box")),
        None,
        0,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("heartbeat");
    assert_eq!(beat.settings_gen, 1, "heartbeat must not bump settings_gen");
    assert_eq!(
        beat.settings_ack_gen, 0,
        "heartbeat without ack must not set settings_ack_gen"
    );
    assert!(beat.settings_pending());

    let second = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            labels: Some(vec!["gpu".into()]),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("second apply");
    assert_eq!(second.settings_gen, 2);

    // HTTP heartbeat composes heartbeat() then ack_settings(body.settings_ack_gen).
    // An old binary still echoing the gen it last saw must not clear pending.
    let beat2 = worker_hosts::heartbeat(
        &pool,
        host.id,
        true,
        false,
        Some("linux-x86_64"),
        Some(&format!("{name}-box")),
        None,
        0,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("heartbeat after second apply");
    assert_eq!(beat2.settings_gen, 2);
    assert_eq!(beat2.settings_ack_gen, 0);

    let err = worker_hosts::ack_settings(&pool, host.id, 1)
        .await
        .expect_err("stale heartbeat ack must not match current settings_gen");
    match err {
        DbError::NotFound(_) => {}
        other => panic!("expected NotFound on stale heartbeat ack, got {other:?}"),
    }

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(loaded.settings_gen, 2);
    assert_eq!(loaded.settings_ack_gen, 0);
    assert!(
        loaded.settings_pending(),
        "heartbeat ack of old gen must leave apply pending"
    );
}

/// catalog:unit-patch-name — PATCH name persists and bumps settings_gen.
#[tokio::test]
async fn patch_name_persists_and_bumps_settings_gen() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, _token) =
        create_pending_with_token(&pool, workspace_id, user_id, "studio", expires).await;
    assert_eq!(host.name, "studio");
    assert_eq!(host.settings_gen, 0);

    let same = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            name: Some("studio".into()),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("same name");
    assert_eq!(same.name, "studio");
    assert_eq!(
        same.settings_gen, 0,
        "identical name must not bump settings_gen"
    );

    let renamed = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            name: Some("renamed-studio".into()),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("rename");
    assert_eq!(renamed.name, "renamed-studio");
    assert_eq!(renamed.settings_gen, 1);
    assert_eq!(renamed.isolation, host.isolation);
    assert!(renamed.settings_pending());

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(loaded.name, "renamed-studio");
    assert_eq!(loaded.settings_gen, 1);

    let listed = worker_hosts::list_for_workspace(&pool, workspace_id)
        .await
        .expect("list");
    let row = listed
        .iter()
        .find(|h| h.id == host.id)
        .expect("list missing renamed host");
    assert_eq!(row.name, "renamed-studio");
}

/// catalog:unit-request-update — Auto-update off + Update now (`request_update`).
#[tokio::test]
async fn request_update_sets_timestamp_without_bumping_gen() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "upd", expires).await;
    pair_host(&pool, &token, "upd-box").await;

    let before = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert!(before.auto_update, "paired host defaults auto-update on");
    assert!(
        before.update_requested_at.is_none(),
        "no update requested yet"
    );
    let settings_gen = before.settings_gen;

    let off = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            auto_update: Some(false),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("auto off");
    assert!(!off.auto_update);
    assert!(
        off.update_requested_at.is_none(),
        "turning auto-update off must not request"
    );
    assert_eq!(off.settings_gen, settings_gen + 1);

    let ignored = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            request_update: Some(false),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("request_update false");
    assert!(ignored.update_requested_at.is_none());
    assert_eq!(ignored.settings_gen, off.settings_gen);

    let requested = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            request_update: Some(true),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("request_update");
    assert!(!requested.auto_update, "auto stays off");
    let at = requested
        .update_requested_at
        .expect("update_requested_at must be set");
    assert_eq!(
        requested.settings_gen, off.settings_gen,
        "Update now is a one-shot, not a settings bump"
    );

    let again = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            request_update: Some(true),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("request again");
    assert!(
        again.update_requested_at.expect("still set") >= at,
        "a second Update now refreshes the timestamp"
    );
    assert_eq!(again.settings_gen, off.settings_gen);
}

#[tokio::test]
async fn expired_pair_token_fails() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expired_at = Utc::now() - Duration::seconds(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "stale-token", expired_at).await;
    assert_pair_token_hashed_at_rest(&pool, host.id, &token).await;

    let err =
        worker_hosts::consume_pair_token(&pool, &token, "box", "linux-x86_64", true, false, None)
            .await
            .expect_err("expired pair token must fail");
    assert_pair_conflict(err, "expired");

    let still = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host remains");
    assert_eq!(still.status, "pending");
    assert!(still.machine_token_hash.is_none());
    assert!(still.paired_at.is_none());
    let (_, used) = pair_token_hash_and_used_at(&pool, host.id, &token).await;
    assert!(used.is_none(), "expired consume must not set used_at");
    assert_pair_token_hashed_at_rest(&pool, host.id, &token).await;

    // Control: an unexpired sibling still pairs (not every consume fails).
    let (live, live_token) = create_pending_with_token(
        &pool,
        workspace_id,
        user_id,
        "live-token",
        Utc::now() + Duration::hours(1),
    )
    .await;
    let live_paired = pair_host(&pool, &live_token, "box-live").await;
    assert_eq!(live_paired.host.id, live.id);
    assert_eq!(live_paired.host.status, "online");
}

#[tokio::test]
async fn revoke_rejects_authenticate() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "revoked", expires).await;
    let paired = pair_host(&pool, &token, "box-rev").await;

    assert!(
        worker_hosts::authenticate_machine(&pool, &paired.machine_token)
            .await
            .expect("auth before revoke")
            .is_some()
    );

    let revoked = worker_hosts::revoke(&pool, host.id).await.expect("revoke");
    assert_eq!(revoked.status, "disabled");
    assert!(revoked.machine_token_hash.is_none());

    assert!(
        worker_hosts::authenticate_machine(&pool, &paired.machine_token)
            .await
            .expect("auth after revoke")
            .is_none(),
        "disabled host must not authenticate"
    );
}

/// After revoke the picker skips the row. Old `flm_` cannot heartbeat or
/// lease. Re-pair is required — this test must not rotate the machine token.
#[tokio::test]
async fn revoked_host_is_not_list_assignable_and_cannot_take_a_lease() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);

    let (target, target_pair) =
        create_pending_with_token(&pool, workspace_id, user_id, "revoke-target", expires).await;
    let paired = pair_host(&pool, &target_pair, "revoke-box").await;
    let old_token = paired.machine_token.clone();
    let before_hb = worker_hosts::heartbeat(
        &pool,
        target.id,
        true,
        false,
        None,
        None,
        None,
        0,
        Some(4),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("heartbeat before revoke");
    let seen_before = before_hb
        .last_seen_at
        .expect("paired+heartbeat sets last_seen_at");
    assert!(
        worker_hosts::authenticate_machine(&pool, &old_token)
            .await
            .expect("auth before revoke")
            .is_some(),
        "paired host must authenticate before revoke"
    );
    assert!(
        before_hb.accepts_v1_leases(),
        "online native host must accept v1 leases before revoke"
    );

    let (live, live_pair) =
        create_pending_with_token(&pool, workspace_id, user_id, "still-live", expires).await;
    let live_paired = pair_host(&pool, &live_pair, "live-box").await;
    worker_hosts::heartbeat(
        &pool,
        live.id,
        true,
        false,
        None,
        None,
        None,
        0,
        Some(4),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("heartbeat live sibling");

    let before = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
        .await
        .expect("list_assignable before revoke");
    let before_ids: Vec<Uuid> = before.iter().map(|h| h.id).collect();
    assert!(
        before_ids.contains(&target.id),
        "fresh paired host must be assignable before revoke: {before_ids:?}"
    );
    assert!(
        before_ids.contains(&live.id),
        "live sibling must be assignable: {before_ids:?}"
    );

    let claim = spawn_claim(workspace_id, target.id, "spawn_session");
    work_claims::enqueue(&pool, &claim)
        .await
        .expect("enqueue spawn for target");

    // Do not rotate_machine_token. Revoke clears the hash; recovery is re-pair.
    let revoked = worker_hosts::revoke(&pool, target.id)
        .await
        .expect("revoke");
    assert_eq!(revoked.status, "disabled");
    assert!(revoked.machine_token_hash.is_none());
    assert!(
        !revoked.accepts_v1_leases(),
        "disabled host must not accept v1 leases"
    );

    let after = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
        .await
        .expect("list_assignable after revoke");
    let after_ids: Vec<Uuid> = after.iter().map(|h| h.id).collect();
    assert!(
        !after_ids.contains(&target.id),
        "revoked host must drop out of list_assignable: {after_ids:?}"
    );
    assert!(
        after_ids.contains(&live.id),
        "live sibling must stay assignable after the other host is revoked: {after_ids:?}"
    );

    assert!(
        worker_hosts::authenticate_machine(&pool, &old_token)
            .await
            .expect("auth after revoke")
            .is_none(),
        "old machine token must not authenticate — /me/lease require_machine fails"
    );
    assert!(
        worker_hosts::authenticate_machine(&pool, &live_paired.machine_token)
            .await
            .expect("auth live sibling")
            .is_some(),
        "sibling machine token must still authenticate"
    );

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let hb_err = worker_hosts::heartbeat(
        &pool,
        target.id,
        true,
        false,
        None,
        None,
        None,
        0,
        Some(4),
        None,
        None,
        None,
        None,
    )
    .await
    .expect_err("heartbeat after revoke must fail");
    match hb_err {
        DbError::NotFound(msg) => assert!(
            msg.contains("not found"),
            "revoked heartbeat must be NotFound, got {msg}"
        ),
        other => panic!("expected NotFound on revoked heartbeat, got {other:?}"),
    }
    let still = worker_hosts::get(&pool, target.id)
        .await
        .expect("get revoked")
        .expect("revoked row remains");
    assert_eq!(still.status, "disabled");
    assert!(still.machine_token_hash.is_none());
    assert_eq!(
        still.last_seen_at,
        Some(seen_before),
        "failed heartbeat must not advance last_seen_at"
    );

    let parked = work_claims::get(&pool, claim.id)
        .await
        .expect("get claim")
        .expect("claim remains");
    assert_eq!(
        parked.status, "ready",
        "revoked host must not have leased the waiting spawn: {:?}",
        parked.status
    );
    assert!(parked.lease_owner.is_none());
    assert!(parked.lease_token.is_none());

    let replace_err = worker_hosts::replace_pair_token(
        &pool,
        target.id,
        &hash_token("flp_must-not-replace-after-revoke"),
        Utc::now() + Duration::hours(1),
    )
    .await
    .expect_err("replace_pair_token only while pending");
    match replace_err {
        DbError::Conflict(msg) => assert!(
            msg.contains("pending"),
            "online/disabled hosts re-pair via a new pending, not replace_pair_token: {msg}"
        ),
        other => panic!("expected Conflict on replace_pair_token after revoke, got {other:?}"),
    }
}

#[tokio::test]
async fn heartbeat_updates_last_seen_at() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "hb", expires).await;
    let paired = pair_host(&pool, &token, "box-hb").await;
    let first_seen = paired.host.last_seen_at.expect("paired sets last_seen_at");

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;

    let after = worker_hosts::heartbeat(
        &pool,
        host.id,
        false,
        true,
        Some("darwin-arm64"),
        Some("box-hb-2"),
        Some("disk full"),
        0,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("heartbeat");

    let seen = after.last_seen_at.expect("heartbeat sets last_seen_at");
    assert!(seen > first_seen, "last_seen_at must advance");
    assert!(!after.advertised_host);
    assert!(after.advertised_container);
    assert_eq!(after.platform.as_deref(), Some("darwin-arm64"));
    assert_eq!(after.hostname.as_deref(), Some("box-hb-2"));
    assert_eq!(after.last_error.as_deref(), Some("disk full"));
    assert_eq!(after.current_jobs, 0);
    assert_eq!(after.advertised_max_concurrent, None);

    let load = worker_hosts::heartbeat(
        &pool,
        host.id,
        false,
        true,
        None,
        None,
        None,
        -3,
        Some(512),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("heartbeat load");
    assert_eq!(load.current_jobs, 0, "current_jobs must clamp >= 0");
    assert_eq!(
        load.advertised_max_concurrent,
        Some(256),
        "advertised_max_concurrent must clamp to 256"
    );

    let mid = worker_hosts::heartbeat(
        &pool,
        host.id,
        false,
        true,
        None,
        None,
        None,
        2,
        Some(8),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("heartbeat mid");
    assert_eq!(mid.current_jobs, 2);
    assert_eq!(mid.advertised_max_concurrent, Some(8));

    let keep = worker_hosts::heartbeat(
        &pool, host.id, false, true, None, None, None, 1, None, None, None, None, None,
    )
    .await
    .expect("heartbeat keep advertised");
    assert_eq!(keep.current_jobs, 1);
    assert_eq!(
        keep.advertised_max_concurrent,
        Some(8),
        "None must leave advertised_max_concurrent unchanged"
    );
    assert!(
        keep.software_git_sha.is_none(),
        "old heartbeat without software must leave software unset"
    );

    let bins = serde_json::json!({ "fleet-host": "abc" });
    let with_sw = worker_hosts::heartbeat(
        &pool,
        host.id,
        false,
        true,
        None,
        None,
        None,
        1,
        None,
        Some("deadbeef"),
        Some(bins.clone()),
        Some(false),
        None,
    )
    .await
    .expect("heartbeat software");
    assert_eq!(with_sw.software_git_sha.as_deref(), Some("deadbeef"));
    assert_eq!(with_sw.software_bins, Some(bins));
    assert!(with_sw.software_reported_at.is_some());
    assert!(!with_sw.software_updating);

    let keep_sw = worker_hosts::heartbeat(
        &pool, host.id, false, true, None, None, None, 1, None, None, None, None, None,
    )
    .await
    .expect("heartbeat keep software");
    assert_eq!(
        keep_sw.software_git_sha.as_deref(),
        Some("deadbeef"),
        "omitted software must not clear the last report"
    );

    let hw =
        serde_json::json!({ "cpu_cores": 8, "mem_total_bytes": 32000000000u64, "os": "linux" });
    let with_hw = worker_hosts::heartbeat(
        &pool,
        host.id,
        false,
        true,
        None,
        None,
        None,
        3,
        None,
        None,
        None,
        None,
        Some(hw.clone()),
    )
    .await
    .expect("heartbeat hardware");
    assert_eq!(with_hw.current_jobs, 3);
    assert_eq!(with_hw.hardware, Some(hw));
    let pub_h = with_hw.as_public(Utc::now());
    assert_eq!(pub_h.current_jobs, 3);
    assert_eq!(pub_h.hardware.as_ref().and_then(|h| h.cpu_cores), Some(8));
}

#[tokio::test]
async fn heartbeat_with_machine_persists_agent_runtimes() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("runtimes");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    let mid = Uuid::now_v7();
    let paired = worker_hosts::consume_pair_token(
        &pool,
        &token,
        &format!("{name}-box"),
        "linux-x86_64",
        true,
        false,
        Some(mid),
    )
    .await
    .expect("pair");
    assert_eq!(
        paired.host.agent_runtimes,
        json!([]),
        "new host defaults to empty agent_runtimes"
    );

    let runtimes = json!([
        {
            "id": "grok",
            "name": "Grok",
            "installed": true,
            "logged_in": true,
            "models": ["grok-4"],
            "model_details": [{
                "name": "grok-4",
                "efforts": [{"value": "high", "label": "High"}],
                "default_effort": "high"
            }]
        },
        {
            "id": "claude",
            "name": "Claude",
            "installed": true,
            "logged_in": false,
            "models": [],
            "model_details": []
        }
    ]);
    let after = heartbeat_with_runtimes(&pool, host.id, Some(mid), Some(runtimes.clone())).await;
    assert_eq!(after.agent_runtimes, runtimes, "Some(json) must persist");
    assert_eq!(
        after.machine_id,
        Some(mid),
        "machine_id bind must not be swapped with agent_runtimes"
    );

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(loaded.agent_runtimes, runtimes);
    assert_eq!(loaded.machine_id, Some(mid));

    let listed = worker_hosts::list_for_workspace(&pool, workspace_id)
        .await
        .expect("list");
    let row = listed
        .iter()
        .find(|h| h.id == host.id)
        .expect("listed host");
    assert_eq!(row.agent_runtimes, runtimes);

    let pub_h = loaded.as_public(Utc::now());
    assert_eq!(pub_h.agents.len(), 2);
    assert_eq!(pub_h.agents[0].id, "grok");
    assert_eq!(pub_h.agents[0].name.as_deref(), Some("Grok"));
    assert!(pub_h.agents[0].installed);
    assert!(pub_h.agents[0].logged_in);
    assert_eq!(pub_h.agents[0].models, vec!["grok-4"]);
    assert_eq!(pub_h.agents[0].model_details.len(), 1);
    assert_eq!(pub_h.agents[0].model_details[0].name, "grok-4");
    assert_eq!(
        pub_h.agents[0].model_details[0].default_effort.as_deref(),
        Some("high")
    );
    assert_eq!(pub_h.agents[1].id, "claude");
    assert!(!pub_h.agents[1].logged_in);

    let replaced = json!([{
        "id": "ollama",
        "name": "Ollama",
        "installed": true,
        "logged_in": false,
        "models": ["llama3"],
        "model_details": []
    }]);
    let again = heartbeat_with_runtimes(&pool, host.id, None, Some(replaced.clone())).await;
    assert_eq!(
        again.agent_runtimes, replaced,
        "a later Some(json) must replace, not merge"
    );
    assert_eq!(
        again.machine_id,
        Some(mid),
        "None machine_id must leave the durable id"
    );
    assert_eq!(again.as_public(Utc::now()).agents.len(), 1);
    assert_eq!(again.as_public(Utc::now()).agents[0].id, "ollama");
}

#[tokio::test]
async fn heartbeat_with_machine_none_does_not_wipe_agent_runtimes() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("keep-rt");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let other_name = unique_slug("other-rt");
    let (other, other_token) =
        create_pending_with_token(&pool, workspace_id, user_id, &other_name, expires).await;
    pair_host(&pool, &other_token, &format!("{other_name}-box")).await;
    let other_runtimes = json!([{
        "id": "openai",
        "name": "OpenAI",
        "installed": true,
        "logged_in": true,
        "models": ["gpt-4"],
        "model_details": []
    }]);
    heartbeat_with_runtimes(&pool, other.id, None, Some(other_runtimes.clone())).await;

    let runtimes = json!([
        {
            "id": "grok",
            "name": "Grok",
            "installed": true,
            "logged_in": true,
            "models": ["grok-4"],
            "model_details": []
        }
    ]);
    heartbeat_with_runtimes(&pool, host.id, None, Some(runtimes.clone())).await;

    let omitted = heartbeat_with_runtimes(&pool, host.id, None, None).await;
    assert_eq!(
        omitted.agent_runtimes, runtimes,
        "None must COALESCE and leave the last report (not reset to [])"
    );

    let via_wrapper = worker_hosts::heartbeat(
        &pool, host.id, true, false, None, None, None, 1, None, None, None, None, None,
    )
    .await
    .expect("legacy heartbeat wrapper");
    assert_eq!(
        via_wrapper.agent_runtimes, runtimes,
        "heartbeat() always passes None and must not wipe agent_runtimes"
    );
    assert_eq!(via_wrapper.current_jobs, 1);

    let cleared = heartbeat_with_runtimes(&pool, host.id, None, Some(json!([]))).await;
    assert_eq!(
        cleared.agent_runtimes,
        json!([]),
        "Some([]) is a real empty report and must persist, unlike omitted None"
    );
    let stay_empty = heartbeat_with_runtimes(&pool, host.id, None, None).await;
    assert_eq!(
        stay_empty.agent_runtimes,
        json!([]),
        "None after an empty report must not restore the previous agents"
    );

    let other_loaded = worker_hosts::get(&pool, other.id)
        .await
        .expect("get other")
        .expect("other host");
    assert_eq!(
        other_loaded.agent_runtimes, other_runtimes,
        "COALESCE on one host must not touch another host's agent_runtimes"
    );
}

/// Catalog specs: heartbeat stores CPU/RAM/GPU inventory. Empty GPU names
/// must not appear on the public card. Omitting `hardware` must COALESCE
/// (keep the last snapshot), not wipe it.
#[tokio::test]
async fn heartbeat_persists_hardware_inventory_empty_gpu_name_dropped() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("hw-inv");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let other_name = unique_slug("hw-other");
    let (other, other_token) =
        create_pending_with_token(&pool, workspace_id, user_id, &other_name, expires).await;
    pair_host(&pool, &other_token, &format!("{other_name}-box")).await;
    let other_hw = json!({
        "cpu_model": "AMD Ryzen 7 7840U",
        "cpu_cores": 16,
        "mem_total_bytes": 34359738368u64,
        "mem_type": "DDR5",
        "gpus": [{"name": "AMD Radeon 780M", "used_pct": 7}],
        "os": "linux 6.8 x86_64"
    });
    heartbeat_with_hardware(&pool, other.id, Some(other_hw.clone())).await;

    // Catalog specs snapshot (test_catalog_specs.SPECS_HARDWARE).
    let inventory = json!({
        "cpu_model": "Intel(R) Core(TM) i7-10610U CPU @ 1.80GHz",
        "cpu_cores": 8,
        "mem_total_bytes": 17179869184u64,
        "mem_speed_mts": 3200,
        "mem_type": "DDR4",
        "gpus": [{"name": "Intel UHD Graphics", "used_pct": 4}],
        "os": "linux 6.8 x86_64",
        "cpu_used_pct": 12,
        "mem_used_pct": 32
    });
    let stored = heartbeat_with_hardware(&pool, host.id, Some(inventory.clone())).await;
    assert_eq!(
        stored.hardware,
        Some(inventory.clone()),
        "Some(hardware) must persist the CPU/RAM/GPU snapshot as-is"
    );

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(loaded.hardware, Some(inventory.clone()));
    let listed = worker_hosts::list_for_workspace(&pool, workspace_id)
        .await
        .expect("list");
    let row = listed
        .iter()
        .find(|h| h.id == host.id)
        .expect("listed host");
    assert_eq!(row.hardware, Some(inventory.clone()));

    let pub_h = loaded.as_public(Utc::now());
    let hw = pub_h.hardware.as_ref().expect("public hardware");
    assert_eq!(hw.cpu_cores, Some(8));
    assert_eq!(hw.mem_total_bytes, Some(17179869184));
    assert_eq!(hw.mem_speed_mts, Some(3200));
    assert_eq!(hw.mem_type.as_deref(), Some("DDR4"));
    assert_eq!(
        hw.cpu_model.as_deref(),
        Some("Intel(R) Core(TM) i7-10610U CPU @ 1.80GHz")
    );
    assert_eq!(hw.cpu_used_pct, Some(12));
    assert_eq!(hw.mem_used_pct, Some(32));
    assert_eq!(hw.os.as_deref(), Some("linux 6.8 x86_64"));
    assert_eq!(public_gpu_names(&loaded), vec!["Intel UHD Graphics"]);
    let gpus = hw.gpus.as_ref().expect("public gpus");
    assert_eq!(gpus.len(), 1, "named iGPU must stay: {gpus:?}");
    assert_eq!(gpus[0].name.as_deref(), Some("Intel UHD Graphics"));
    assert_eq!(gpus[0].used_pct, Some(4));

    // Empty / whitespace / trademark-only / missing names are not inventory.
    let with_empties = json!({
        "cpu_model": "Intel(R) Core(TM) i7-10610U CPU @ 1.80GHz",
        "cpu_cores": 8,
        "mem_total_bytes": 17179869184u64,
        "mem_type": "DDR4",
        "gpus": [
            {"name": ""},
            {"name": "   "},
            {"name": "(R)"},
            {"name": null},
            {},
            {"name": "Intel UHD Graphics", "used_pct": 4}
        ]
    });
    let mixed = heartbeat_with_hardware(&pool, host.id, Some(with_empties)).await;
    let persisted_gpus = persisted_gpu_entries(&mixed);
    assert!(
        persisted_gpus
            .iter()
            .all(|g| gpu_entry_name(g).is_some_and(|n| !n.trim().is_empty())),
        "persisted GPU inventory must drop empty/whitespace/null names, not store them: {persisted_gpus:?}"
    );
    assert!(
        persisted_gpus
            .iter()
            .any(|g| gpu_entry_name(g) == Some("Intel UHD Graphics")),
        "named iGPU must remain in persisted inventory: {persisted_gpus:?}"
    );
    let mixed_names = public_gpu_names(&mixed);
    assert_eq!(
        mixed_names,
        vec!["Intel UHD Graphics"],
        "empty GPU names must be dropped from the public inventory, named iGPU kept: {mixed_names:?}"
    );

    // Replace, not merge: a later snapshot without mem_type must not keep DDR4.
    let replaced = json!({
        "cpu_model": "Intel(R) Core(TM) i7-10610U CPU @ 1.80GHz",
        "cpu_cores": 4,
        "mem_total_bytes": 8589934592u64,
        "gpus": [{"name": "Intel UHD Graphics"}]
    });
    let after_replace = heartbeat_with_hardware(&pool, host.id, Some(replaced.clone())).await;
    let replaced_hw = after_replace.hardware.as_ref().expect("replaced hardware");
    assert_eq!(replaced_hw.get("cpu_cores"), Some(&json!(4)));
    assert!(
        replaced_hw.get("mem_type").is_none(),
        "later Some(json) must replace the snapshot (not merge leftover keys): {replaced_hw}"
    );
    assert_eq!(
        after_replace.hardware,
        Some(replaced),
        "replace snapshot must persist exactly (empty GPU names already stripped by the caller)"
    );

    // Only nameless GPUs + catalog CPU → drop empties, infer Intel UHD on the card.
    let only_empty = json!({
        "cpu_model": "Intel(R) Core(TM) i7-10610U CPU @ 1.80GHz",
        "cpu_cores": 8,
        "mem_total_bytes": 17179869184u64,
        "gpus": [{"name": ""}, {"name": "  "}, {"name": null}]
    });
    let inferred = heartbeat_with_hardware(&pool, host.id, Some(only_empty)).await;
    let inferred_gpus = persisted_gpu_entries(&inferred);
    assert!(
        inferred_gpus
            .iter()
            .all(|g| gpu_entry_name(g).is_some_and(|n| !n.trim().is_empty())),
        "persisted inventory must not keep nameless GPUs: {inferred_gpus:?}"
    );
    assert_eq!(
        public_gpu_names(&inferred),
        vec!["Intel UHD Graphics"],
        "after empty GPU names are dropped, catalog CPU must still surface the iGPU"
    );

    // Restore the named inventory, then omit hardware — COALESCE must keep it.
    heartbeat_with_hardware(&pool, host.id, Some(inventory.clone())).await;
    let omitted = heartbeat_with_hardware(&pool, host.id, None).await;
    assert_eq!(
        omitted.hardware,
        Some(inventory.clone()),
        "None must COALESCE and leave the last CPU/RAM/GPU inventory (not wipe to null)"
    );
    assert_eq!(public_gpu_names(&omitted), vec!["Intel UHD Graphics"]);

    let via_wrapper = worker_hosts::heartbeat(
        &pool, host.id, true, false, None, None, None, 1, None, None, None, None, None,
    )
    .await
    .expect("legacy heartbeat wrapper");
    assert_eq!(
        via_wrapper.hardware,
        Some(inventory.clone()),
        "heartbeat() with omitted hardware must not wipe inventory"
    );
    assert_eq!(via_wrapper.current_jobs, 1);

    // Bind order: hardware $13 vs agent_runtimes $15. Updating runtimes
    // with hardware=None must not clear inventory.
    let runtimes = json!([{
        "id": "grok",
        "name": "Grok",
        "installed": true,
        "logged_in": true,
        "models": ["grok-4"],
        "model_details": []
    }]);
    let with_both = worker_hosts::heartbeat_with_machine(
        &pool,
        host.id,
        true,
        false,
        None,
        None,
        None,
        0,
        None,
        None,
        None,
        None,
        Some(inventory.clone()),
        None,
        Some(runtimes.clone()),
    )
    .await
    .expect("heartbeat hardware+runtimes");
    assert_eq!(with_both.hardware, Some(inventory.clone()));
    assert_eq!(with_both.agent_runtimes, runtimes);

    let runtimes_only = worker_hosts::heartbeat_with_machine(
        &pool,
        host.id,
        true,
        false,
        None,
        None,
        None,
        0,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(json!([])),
    )
    .await
    .expect("heartbeat runtimes only");
    assert_eq!(
        runtimes_only.hardware,
        Some(inventory.clone()),
        "omitted hardware must not be overwritten by the agent_runtimes bind"
    );
    assert_eq!(runtimes_only.agent_runtimes, json!([]));

    let other_loaded = worker_hosts::get(&pool, other.id)
        .await
        .expect("get other")
        .expect("other host");
    assert_eq!(
        other_loaded.hardware,
        Some(other_hw),
        "COALESCE on one host must not touch another host's hardware"
    );
    assert_eq!(
        public_gpu_names(&other_loaded),
        vec!["AMD Radeon 780M"],
        "other host public GPU must stay"
    );

    // Some({}) is an explicit empty snapshot and must persist, unlike omitted None.
    let cleared = heartbeat_with_hardware(&pool, host.id, Some(json!({}))).await;
    assert_eq!(
        cleared.hardware,
        Some(json!({})),
        "Some({{}}) is a real empty report and must persist, unlike omitted None"
    );
    let stay_empty = heartbeat_with_hardware(&pool, host.id, None).await;
    assert_eq!(
        stay_empty.hardware,
        Some(json!({})),
        "None after an empty hardware report must not restore the previous inventory"
    );
}

#[tokio::test]
async fn list_assignable_excludes_paused_disabled_stale() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);

    let (ok_host, ok_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "ok", expires).await;
    pair_host(&pool, &ok_token, "ok-box").await;

    let (paused_host, paused_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "paused", expires).await;
    pair_host(&pool, &paused_token, "paused-box").await;
    worker_hosts::patch_cp_settings(
        &pool,
        paused_host.id,
        &HostCpSettings {
            paused: Some(true),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("pause");

    let (drain_host, drain_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "drain", expires).await;
    pair_host(&pool, &drain_token, "drain-box").await;
    worker_hosts::patch_cp_settings(
        &pool,
        drain_host.id,
        &HostCpSettings {
            drain: Some(true),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("drain");

    let (disabled_host, disabled_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "disabled", expires).await;
    pair_host(&pool, &disabled_token, "disabled-box").await;
    worker_hosts::revoke(&pool, disabled_host.id)
        .await
        .expect("revoke");

    let (off_host, off_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "off", expires).await;
    pair_host(&pool, &off_token, "off-box").await;
    worker_hosts::patch_cp_settings(
        &pool,
        off_host.id,
        &HostCpSettings {
            enabled: Some(false),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("disable enabled flag");

    let (stale_host, stale_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "stale", expires).await;
    pair_host(&pool, &stale_token, "stale-box").await;
    sqlx::query("UPDATE worker_hosts SET last_seen_at = now() - interval '1 hour' WHERE id = $1")
        .bind(stale_host.id)
        .execute(&pool)
        .await
        .expect("age last_seen_at");

    let (_pending, _) =
        create_pending_with_token(&pool, workspace_id, user_id, "still-pending", expires).await;

    let assignable = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
        .await
        .expect("list_assignable");
    let ids = assignable_ids_in_workspace(&assignable, workspace_id);
    assert_eq!(
        ids,
        vec![ok_host.id],
        "only the fresh online host is assignable"
    );
}

/// `holding_new_jobs` (Stop taking new jobs) must drop the host from the picker.
/// A running job may finish; the host must not receive a new lease.
/// Matches fleet-runners-view: paused || drain || !(allow_host || allow_container).
#[tokio::test]
async fn list_assignable_skips_holding_new_jobs_even_with_running_job() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);

    async fn pair_under_cap_with_job(
        pool: &fleet_db::PgPool,
        workspace_id: Uuid,
        user_id: Uuid,
        expires: chrono::DateTime<Utc>,
        name: &str,
    ) -> Uuid {
        let (host, token) =
            create_pending_with_token(pool, workspace_id, user_id, name, expires).await;
        pair_host(pool, &token, &format!("{name}-box")).await;
        let beat = worker_hosts::heartbeat(
            pool, host.id, true, false, None, None, None, 1, None, None, None, None, None,
        )
        .await
        .expect("heartbeat current_jobs=1");
        assert_eq!(beat.current_jobs, 1);
        assert!(
            !worker_hosts::at_job_cap(&beat),
            "{name} default cap is 4; 1 job must not be an at_job_cap skip"
        );
        host.id
    }

    let taking = pair_under_cap_with_job(&pool, workspace_id, user_id, expires, "taking").await;
    let paused =
        pair_under_cap_with_job(&pool, workspace_id, user_id, expires, "paused-hold").await;
    let drain = pair_under_cap_with_job(&pool, workspace_id, user_id, expires, "drain-hold").await;
    let no_modes =
        pair_under_cap_with_job(&pool, workspace_id, user_id, expires, "no-modes-hold").await;

    // UI Stop taking new jobs PATCHes paused=true. Drain is the same hold.
    // Both plane modes off is also holding_new_jobs (!accepting_jobs).
    worker_hosts::patch_cp_settings(
        &pool,
        paused,
        &HostCpSettings {
            paused: Some(true),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("stop taking new jobs (paused)");
    worker_hosts::patch_cp_settings(
        &pool,
        drain,
        &HostCpSettings {
            drain: Some(true),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("drain");
    worker_hosts::patch_cp_settings(
        &pool,
        no_modes,
        &HostCpSettings {
            allow_host: Some(false),
            allow_container: Some(false),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("both modes off");

    let taking_row = worker_hosts::get(&pool, taking)
        .await
        .expect("get taking")
        .expect("taking exists");
    assert!(
        !worker_hosts::holding_new_jobs(&taking_row),
        "control host must still be taking jobs"
    );

    for (id, label) in [(paused, "paused"), (drain, "drain"), (no_modes, "no-modes")] {
        let h = worker_hosts::get(&pool, id)
            .await
            .expect("get hold")
            .expect("hold exists");
        assert!(
            worker_hosts::holding_new_jobs(&h),
            "{label} must match holding_new_jobs (paused || drain || !accepting)"
        );
        assert_eq!(
            h.status, "online",
            "{label} must stay online so the running job can finish"
        );
        assert_eq!(
            h.current_jobs, 1,
            "{label} running job must not be zeroed by the hold"
        );
        assert!(
            !worker_hosts::at_job_cap(&h),
            "{label} is under cap; skip must be holding_new_jobs, not at_job_cap"
        );
    }

    let listed = worker_hosts::list_for_workspace(&pool, workspace_id)
        .await
        .expect("list_for_workspace");
    let listed_ids: Vec<Uuid> = listed.iter().map(|h| h.id).collect();
    assert!(
        listed_ids.contains(&taking)
            && listed_ids.contains(&paused)
            && listed_ids.contains(&drain)
            && listed_ids.contains(&no_modes),
        "hold must not delete the row (running job may finish): {listed_ids:?}"
    );

    let assignable = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
        .await
        .expect("list_assignable");
    let ids = assignable_ids_in_workspace(&assignable, workspace_id);
    assert!(
        ids.contains(&taking),
        "accepting host with a running job under cap must stay assignable: {ids:?}"
    );
    assert!(
        !ids.contains(&paused),
        "Stop taking new jobs (paused) must skip the picker even with a running job: {ids:?}"
    );
    assert!(
        !ids.contains(&drain),
        "drain must skip the picker even with a running job: {ids:?}"
    );
    assert!(
        !ids.contains(&no_modes),
        "holding_new_jobs (!allow_host && !allow_container) must skip the picker; \
         no new lease: {ids:?}"
    );
    assert_eq!(
        ids,
        vec![taking],
        "only the host still taking jobs is assignable: {ids:?}"
    );

    let resumed = worker_hosts::patch_cp_settings(
        &pool,
        paused,
        &HostCpSettings {
            paused: Some(false),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("take jobs again");
    assert!(!resumed.paused);
    assert_eq!(resumed.current_jobs, 1);
    let after = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
        .await
        .expect("list_assignable after resume");
    let after_ids: Vec<Uuid> = after.iter().map(|h| h.id).collect();
    assert!(
        after_ids.contains(&paused) && after_ids.contains(&taking),
        "Take jobs again must return the host to the picker: {after_ids:?}"
    );
    assert!(
        !after_ids.contains(&drain) && !after_ids.contains(&no_modes),
        "other holds must stay out of the picker: {after_ids:?}"
    );
}

#[tokio::test]
async fn list_assignable_includes_cluster_host_from_other_workspace() {
    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (_user_b, ws_b) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, ws_a, user_a, "cluster-box", expires).await;
    pair_host(&pool, &token, "cluster-box").await;
    sqlx::query("UPDATE worker_hosts SET labels = array['cluster'] WHERE id = $1")
        .bind(host.id)
        .execute(&pool)
        .await
        .expect("label cluster");
    let from_b = worker_hosts::list_assignable(&pool, ws_b, Duration::seconds(30))
        .await
        .expect("list_assignable B");
    assert!(
        from_b.iter().any(|h| h.id == host.id),
        "cluster host home={ws_a} must be assignable in {ws_b}: {:?}",
        from_b.iter().map(|h| h.id).collect::<Vec<_>>()
    );
}

/// Stored `CLUSTER` / `Cluster` must appear in list_cluster and unscope
/// list_assignable the same way `is_cluster_host` does.
#[tokio::test]
async fn list_cluster_and_assignable_match_cluster_label_case() {
    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (_user_b, ws_b) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, ws_a, user_a, "cluster-case", expires).await;
    pair_host(&pool, &token, "cluster-case").await;
    sqlx::query("UPDATE worker_hosts SET labels = array[' CLUSTER '] WHERE id = $1")
        .bind(host.id)
        .execute(&pool)
        .await
        .expect("stamp CLUSTER");

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert!(
        loaded.is_cluster_host(),
        "is_cluster_host must treat CLUSTER as bootstrap: {:?}",
        loaded.labels
    );

    let clustered = worker_hosts::list_cluster(&pool)
        .await
        .expect("list_cluster");
    assert!(
        clustered.iter().any(|h| h.id == host.id),
        "CLUSTER label must appear in list_cluster: {:?}",
        clustered.iter().map(|h| h.id).collect::<Vec<_>>()
    );

    let from_b = worker_hosts::list_assignable(&pool, ws_b, Duration::seconds(30))
        .await
        .expect("list_assignable B");
    assert!(
        from_b.iter().any(|h| h.id == host.id),
        "CLUSTER host home={ws_a} must be assignable in {ws_b}: {:?}",
        from_b.iter().map(|h| h.id).collect::<Vec<_>>()
    );
}

/// Write paths store the cluster token as lowercase `cluster`.
#[tokio::test]
async fn create_pending_and_patch_normalize_cluster_label() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let mut new = NewWorkerHost::new(
        Uuid::now_v7(),
        workspace_id,
        format!("norm-cluster-{}", Uuid::now_v7()),
        Some(user_id),
    );
    new.labels = vec![" CLUSTER ".into(), "gpu".into()];
    let created = worker_hosts::create_pending(
        &pool,
        &new,
        &hash_token(&generate_pair_token()),
        expires,
    )
    .await
    .expect("create_pending");
    assert_eq!(
        created.labels,
        vec!["cluster".to_string(), "gpu".to_string()],
        "create_pending must normalize cluster case/whitespace"
    );

    let (host, _token) =
        create_pending_with_token(&pool, workspace_id, user_id, "patch-cluster", expires).await;
    let patched = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            labels: Some(vec!["Cluster".into(), " linux ".into()]),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("patch_cp_settings");
    assert_eq!(
        patched.labels,
        vec!["cluster".to_string(), "linux".to_string()],
        "patch_cp_settings must normalize cluster case and trim"
    );
    let clustered = worker_hosts::list_cluster(&pool)
        .await
        .expect("list_cluster");
    assert!(
        clustered.iter().any(|h| h.id == host.id),
        "normalized cluster label must appear in list_cluster"
    );
}

/// 022 inferred `cluster` on container-only UI-paired boxes. Clearing that
/// label must drop cross-workspace assignability; home workspace stays.
#[tokio::test]
async fn clearing_inferred_cluster_label_removes_cross_workspace_assignability() {
    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (_user_b, ws_b) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);

    let (host, token) =
        create_pending_with_token(&pool, ws_a, user_a, "inferred-cluster", expires).await;
    let paired = worker_hosts::consume_pair_token(
        &pool,
        &token,
        "inferred-cluster",
        "linux-x86_64",
        false,
        true,
        None,
    )
    .await
    .expect("pair container-only (022 inference shape)");
    assert_eq!(paired.host.id, host.id);
    assert_eq!(paired.host.created_by, Some(user_a));
    assert!(
        !paired.host.advertised_host && paired.host.advertised_container,
        "022 inferred cluster on container-only advertised boxes"
    );

    let labeled = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            labels: Some(vec!["cluster".into(), "gpu".into()]),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("stamp inferred cluster");
    assert!(
        labeled.labels.iter().any(|l| l == "cluster"),
        "cluster label must be present before undo: {:?}",
        labeled.labels
    );

    let from_b = worker_hosts::list_assignable(&pool, ws_b, Duration::seconds(30))
        .await
        .expect("list_assignable B with cluster");
    assert!(
        from_b.iter().any(|h| h.id == host.id),
        "cluster host home={ws_a} must be assignable in {ws_b} before undo: {:?}",
        from_b.iter().map(|h| h.id).collect::<Vec<_>>()
    );
    let clustered = worker_hosts::list_cluster(&pool)
        .await
        .expect("list_cluster before undo");
    assert!(
        clustered.iter().any(|h| h.id == host.id),
        "cluster label must appear in list_cluster before undo"
    );

    let cleared = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            labels: Some(vec!["gpu".into()]),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("clear inferred cluster");
    assert_eq!(
        cleared.labels,
        vec!["gpu".to_string()],
        "undo must drop cluster and keep other labels"
    );
    assert_eq!(cleared.workspace_id, ws_a);

    let from_b_after = worker_hosts::list_assignable(&pool, ws_b, Duration::seconds(30))
        .await
        .expect("list_assignable B after undo");
    assert!(
        from_b_after.iter().all(|h| h.id != host.id),
        "cleared inferred cluster host home={ws_a} must not be assignable in {ws_b}: {:?}",
        from_b_after.iter().map(|h| h.id).collect::<Vec<_>>()
    );

    let from_a = worker_hosts::list_assignable(&pool, ws_a, Duration::seconds(30))
        .await
        .expect("list_assignable A after undo");
    assert!(
        from_a.iter().any(|h| h.id == host.id),
        "home workspace must still assign the host after clearing cluster: {:?}",
        from_a.iter().map(|h| h.id).collect::<Vec<_>>()
    );

    let clustered_after = worker_hosts::list_cluster(&pool)
        .await
        .expect("list_cluster after undo");
    assert!(
        clustered_after.iter().all(|h| h.id != host.id),
        "cleared host must not remain a cluster box: {:?}",
        clustered_after.iter().map(|h| h.id).collect::<Vec<_>>()
    );
}

/// 022 stamped `cluster` on every container-only advertised box. 023 undoes
/// that for UI-paired hosts (`created_by` set). Bootstrap (`created_by IS NULL`)
/// keeps the label. Pairing a laptop must not stamp `cluster`.
#[tokio::test]
async fn cluster_label_only_from_bootstrap_null_created_by_migration_023() {
    const OVER_LABEL: &str = include_str!("../migrations/022_cluster_host_label.sql");
    const UNDO: &str = include_str!("../migrations/023_undo_inferred_cluster_label.sql");
    assert!(
        UNDO.contains("created_by IS NOT NULL"),
        "023 must key off created_by IS NOT NULL so bootstrap (NULL) is kept"
    );
    assert!(
        UNDO.contains("array_remove(labels, 'cluster')"),
        "023 must strip only the inferred cluster label"
    );

    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (_user_b, ws_b) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);

    let (laptop_pending, laptop_token) =
        create_pending_with_token(&pool, ws_a, user_a, "paired-laptop", expires).await;
    let laptop = pair_host(&pool, &laptop_token, "paired-laptop").await;
    assert_eq!(laptop.host.id, laptop_pending.id);
    assert_eq!(laptop.host.created_by, Some(user_a));
    assert!(
        laptop.host.advertised_host && !laptop.host.advertised_container,
        "laptop pair shape: advertised_host, not container-only"
    );
    assert!(
        !laptop.host.labels.iter().any(|l| l == "cluster"),
        "pair must not stamp cluster on a laptop: {:?}",
        laptop.host.labels
    );

    let (ui_pending, ui_token) =
        create_pending_with_token(&pool, ws_a, user_a, "ui-container", expires).await;
    let ui_paired = worker_hosts::consume_pair_token(
        &pool,
        &ui_token,
        "ui-container",
        "linux-x86_64",
        false,
        true,
        None,
    )
    .await
    .expect("pair container-only UI box");
    assert_eq!(ui_paired.host.id, ui_pending.id);
    assert_eq!(ui_paired.host.created_by, Some(user_a));
    assert!(
        !ui_paired.host.advertised_host && ui_paired.host.advertised_container,
        "022 victim shape: container-only advertised"
    );
    assert!(
        !ui_paired.host.labels.iter().any(|l| l == "cluster"),
        "pair must not stamp cluster on a UI-paired box: {:?}",
        ui_paired.host.labels
    );
    let ui = worker_hosts::patch_cp_settings(
        &pool,
        ui_paired.host.id,
        &HostCpSettings {
            labels: Some(vec!["gpu".into()]),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("stamp gpu on UI box");
    assert_eq!(ui.labels, vec!["gpu".to_string()]);

    let boot =
        create_bootstrap_cluster_host(&pool, ws_a, "kind-bootstrap", expires, false, true).await;
    assert!(
        boot.host.created_by.is_none(),
        "bootstrap must keep created_by IS NULL"
    );
    assert_eq!(boot.host.labels, vec!["cluster".to_string()]);
    assert!(!boot.host.advertised_host && boot.host.advertised_container);

    let ids = [laptop.host.id, ui.id, boot.host.id];
    apply_label_migration_to_hosts(&pool, OVER_LABEL, &ids).await;

    let laptop_022 = worker_hosts::get(&pool, laptop.host.id)
        .await
        .expect("get laptop after 022")
        .expect("laptop exists");
    assert_eq!(laptop_022.created_by, Some(user_a));
    assert!(
        !laptop_022.labels.iter().any(|l| l == "cluster"),
        "022 must not label a host-advertising laptop: {:?}",
        laptop_022.labels
    );

    let ui_022 = worker_hosts::get(&pool, ui.id)
        .await
        .expect("get UI after 022")
        .expect("UI exists");
    assert_eq!(ui_022.created_by, Some(user_a));
    assert!(
        ui_022.labels.iter().any(|l| l == "cluster") && ui_022.labels.iter().any(|l| l == "gpu"),
        "022 over-labeled container-only UI-paired boxes: {:?}",
        ui_022.labels
    );

    let boot_022 = worker_hosts::get(&pool, boot.host.id)
        .await
        .expect("get bootstrap after 022")
        .expect("bootstrap exists");
    assert!(boot_022.created_by.is_none());
    assert!(
        boot_022.labels.iter().any(|l| l == "cluster"),
        "bootstrap must still have cluster after 022: {:?}",
        boot_022.labels
    );

    let clustered_022 = worker_hosts::list_cluster(&pool)
        .await
        .expect("list_cluster after 022");
    assert!(
        clustered_022.iter().any(|h| h.id == ui.id),
        "022-inferred UI box must appear in list_cluster"
    );
    assert!(
        clustered_022.iter().any(|h| h.id == boot.host.id),
        "bootstrap must appear in list_cluster"
    );
    assert!(
        clustered_022.iter().all(|h| h.id != laptop.host.id),
        "laptop must not appear in list_cluster: {:?}",
        clustered_022.iter().map(|h| h.id).collect::<Vec<_>>()
    );

    let from_b_022 = worker_hosts::list_assignable(&pool, ws_b, Duration::seconds(30))
        .await
        .expect("list_assignable B after 022");
    assert!(
        from_b_022.iter().any(|h| h.id == ui.id),
        "022-inferred cluster made the UI box assignable in {ws_b}"
    );
    assert!(
        from_b_022.iter().any(|h| h.id == boot.host.id),
        "bootstrap must stay assignable in {ws_b}"
    );
    assert!(
        from_b_022.iter().all(|h| h.id != laptop.host.id),
        "laptop home={ws_a} must not be assignable in {ws_b}"
    );

    apply_label_migration_to_hosts(&pool, UNDO, &ids).await;

    let laptop_023 = worker_hosts::get(&pool, laptop.host.id)
        .await
        .expect("get laptop after 023")
        .expect("laptop exists");
    assert_eq!(laptop_023.created_by, Some(user_a));
    assert!(
        !laptop_023.labels.iter().any(|l| l == "cluster"),
        "023 must not stamp cluster on a paired laptop: {:?}",
        laptop_023.labels
    );

    let ui_023 = worker_hosts::get(&pool, ui.id)
        .await
        .expect("get UI after 023")
        .expect("UI exists");
    assert_eq!(ui_023.created_by, Some(user_a));
    assert_eq!(
        ui_023.labels,
        vec!["gpu".to_string()],
        "023 must drop inferred cluster and keep other labels"
    );
    assert_eq!(ui_023.workspace_id, ws_a);

    let boot_023 = worker_hosts::get(&pool, boot.host.id)
        .await
        .expect("get bootstrap after 023")
        .expect("bootstrap exists");
    assert!(
        boot_023.created_by.is_none(),
        "023 must not invent created_by on bootstrap"
    );
    assert!(
        boot_023.labels.iter().any(|l| l == "cluster"),
        "023 must keep cluster on created_by IS NULL bootstrap: {:?}",
        boot_023.labels
    );

    let clustered_023 = worker_hosts::list_cluster(&pool)
        .await
        .expect("list_cluster after 023");
    assert!(
        clustered_023.iter().any(|h| h.id == boot.host.id),
        "bootstrap must remain the only cluster box from this fixture"
    );
    assert!(
        clustered_023
            .iter()
            .all(|h| h.id != ui.id && h.id != laptop.host.id),
        "023 must drop inferred cluster from UI-paired hosts; laptop never had it: {:?}",
        clustered_023.iter().map(|h| h.id).collect::<Vec<_>>()
    );

    let from_b_023 = worker_hosts::list_assignable(&pool, ws_b, Duration::seconds(30))
        .await
        .expect("list_assignable B after 023");
    assert!(
        from_b_023.iter().any(|h| h.id == boot.host.id),
        "bootstrap created_by IS NULL must stay assignable in every workspace"
    );
    assert!(
        from_b_023
            .iter()
            .all(|h| h.id != ui.id && h.id != laptop.host.id),
        "paired laptop / UI box home={ws_a} must not be assignable in {ws_b}: {:?}",
        from_b_023.iter().map(|h| h.id).collect::<Vec<_>>()
    );

    let from_a_023 = worker_hosts::list_assignable(&pool, ws_a, Duration::seconds(30))
        .await
        .expect("list_assignable A after 023");
    assert!(
        from_a_023.iter().any(|h| h.id == laptop.host.id)
            && from_a_023.iter().any(|h| h.id == ui.id)
            && from_a_023.iter().any(|h| h.id == boot.host.id),
        "home workspace must still assign laptop, UI box, and bootstrap: {:?}",
        from_a_023.iter().map(|h| h.id).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn list_assignable_excludes_sandboxed_isolation() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);

    let (native_host, native_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "native", expires).await;
    pair_host(&pool, &native_token, "native-box").await;
    assert_eq!(
        worker_hosts::get(&pool, native_host.id)
            .await
            .expect("get native")
            .expect("native exists")
            .isolation,
        "native"
    );

    let (sandboxed_host, sandboxed_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "sandboxed", expires).await;
    pair_host(&pool, &sandboxed_token, "sandboxed-box").await;
    let patched = worker_hosts::patch_cp_settings(
        &pool,
        sandboxed_host.id,
        &HostCpSettings {
            isolation: Some("sandboxed".into()),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("mark sandboxed");
    assert_eq!(patched.isolation, "sandboxed");
    assert!(!patched.is_native_isolation());

    let assignable = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
        .await
        .expect("list_assignable");
    let ids = assignable_ids_in_workspace(&assignable, workspace_id);
    assert_eq!(
        ids,
        vec![native_host.id],
        "sandboxed host is a placeholder and must not be assignable: {ids:?}"
    );
    assert!(assignable.iter().all(|h| h.isolation == "native"));

    let back = worker_hosts::patch_cp_settings(
        &pool,
        sandboxed_host.id,
        &HostCpSettings {
            isolation: Some("native".into()),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("restore native");
    assert_eq!(back.isolation, "native");
    let both = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
        .await
        .expect("list after restore");
    let both_ids = assignable_ids_in_workspace(&both, workspace_id);
    assert_eq!(
        both_ids.len(),
        2,
        "native again is assignable: {both_ids:?}"
    );
    assert!(both_ids.contains(&native_host.id));
    assert!(both_ids.contains(&sandboxed_host.id));
}

#[tokio::test]
async fn list_updating_recent_is_paired_updating_and_fresh() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let other_ws = seed_user_with_workspace(&pool).await.1;
    let expires = Utc::now() + Duration::hours(1);

    let (fresh, tok) =
        create_pending_with_token(&pool, workspace_id, user_id, "updating", expires).await;
    pair_host(&pool, &tok, "updating-box").await;
    worker_hosts::heartbeat(
        &pool,
        fresh.id,
        true,
        false,
        None,
        None,
        None,
        0,
        None,
        Some("abc"),
        None,
        Some(true),
        None,
    )
    .await
    .expect("hb updating");

    let (stale, stale_tok) =
        create_pending_with_token(&pool, workspace_id, user_id, "stale-up", expires).await;
    pair_host(&pool, &stale_tok, "stale-up-box").await;
    worker_hosts::heartbeat(
        &pool,
        stale.id,
        true,
        false,
        None,
        None,
        None,
        0,
        None,
        Some("abc"),
        None,
        Some(true),
        None,
    )
    .await
    .expect("hb stale updating");
    sqlx::query(
        "UPDATE worker_hosts SET last_seen_at = now() - interval '10 minutes' WHERE id = $1",
    )
    .bind(stale.id)
    .execute(&pool)
    .await
    .expect("age stale updating");

    let (idle, idle_tok) =
        create_pending_with_token(&pool, workspace_id, user_id, "idle", expires).await;
    pair_host(&pool, &idle_tok, "idle-box").await;
    worker_hosts::heartbeat(
        &pool,
        idle.id,
        true,
        false,
        None,
        None,
        None,
        0,
        None,
        Some("abc"),
        None,
        Some(false),
        None,
    )
    .await
    .expect("hb idle");

    let (other, other_tok) =
        create_pending_with_token(&pool, other_ws, user_id, "other-up", expires).await;
    pair_host(&pool, &other_tok, "other-box").await;
    worker_hosts::heartbeat(
        &pool,
        other.id,
        true,
        false,
        None,
        None,
        None,
        0,
        None,
        Some("abc"),
        None,
        Some(true),
        None,
    )
    .await
    .expect("hb other");

    let listed = worker_hosts::list_updating_recent(&pool, workspace_id, Duration::seconds(120))
        .await
        .expect("list_updating_recent");
    let ids: Vec<Uuid> = listed.iter().map(|h| h.id).collect();
    assert_eq!(
        ids,
        vec![fresh.id],
        "only the recent updating host: {ids:?}"
    );
    assert!(listed.iter().all(|h| h.software_updating));
}

fn spawn_claim(workspace_id: Uuid, host_id: Uuid, kind: &str) -> NewWorkClaim {
    NewWorkClaim {
        id: Uuid::now_v7(),
        workspace_id,
        kind: kind.to_string(),
        payload: json!({ "host_id": host_id }),
        max_attempts: 3,
        available_at: Utc::now(),
    }
}

#[tokio::test]
async fn claim_next_spawn_for_host_is_scoped_to_host_and_kind() {
    let pool = connect_and_migrate().await;
    let (_user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let host_a = Uuid::now_v7();
    let host_b = Uuid::now_v7();

    let claim_a = spawn_claim(workspace_id, host_a, "spawn_session");
    let claim_b = spawn_claim(workspace_id, host_b, "spawn_session");
    let other_kind = spawn_claim(workspace_id, host_a, "other_kind");
    work_claims::enqueue(&pool, &claim_a)
        .await
        .expect("enqueue A");
    work_claims::enqueue(&pool, &claim_b)
        .await
        .expect("enqueue B");
    work_claims::enqueue(&pool, &other_kind)
        .await
        .expect("enqueue other kind");

    let got_a =
        work_claims::claim_next_spawn_for_host(&pool, workspace_id, host_a, Duration::minutes(2))
            .await
            .expect("claim A")
            .expect("host A must get its spawn_session");
    assert_eq!(got_a.id, claim_a.id);
    assert_eq!(got_a.lease_owner, Some(host_a));
    assert_eq!(got_a.kind, "spawn_session");

    let again_a =
        work_claims::claim_next_spawn_for_host(&pool, workspace_id, host_a, Duration::minutes(2))
            .await
            .expect("second claim A");
    assert!(
        again_a.is_none(),
        "host A must not receive host B or non-spawn_session claims"
    );

    let got_b =
        work_claims::claim_next_spawn_for_host(&pool, workspace_id, host_b, Duration::minutes(2))
            .await
            .expect("claim B")
            .expect("host B must get its spawn_session");
    assert_eq!(got_b.id, claim_b.id);
    assert_eq!(got_b.lease_owner, Some(host_b));

    let leftover = work_claims::claim_next(&pool, workspace_id, host_a, Duration::minutes(2))
        .await
        .expect("generic claim")
        .expect("other kind remains on the generic path");
    assert_eq!(leftover.id, other_kind.id);
}

/// Regular paired hosts must not lease another workspace's spawn just because
/// `payload->>'host_id'` matches. Unscoped claim is the cluster path.
#[tokio::test]
async fn claim_next_spawn_for_host_in_workspace_does_not_lease_foreign_workspace() {
    let pool = connect_and_migrate().await;
    let ws_a = seed_user_with_workspace(&pool).await.1;
    let ws_b = seed_user_with_workspace(&pool).await.1;
    let host_a = Uuid::now_v7();

    let foreign = spawn_claim(ws_b, host_a, "spawn_session");
    work_claims::enqueue(&pool, &foreign)
        .await
        .expect("enqueue foreign spawn");

    let scoped = work_claims::claim_next_spawn_for_host_in_workspace(
        &pool,
        ws_a,
        host_a,
        Duration::minutes(2),
    )
    .await
    .expect("scoped claim");
    assert!(
        scoped.is_none(),
        "home-workspace claim must not take workspace B spawn: {scoped:?}"
    );

    let unscoped =
        work_claims::claim_next_spawn_for_host(&pool, ws_a, host_a, Duration::minutes(2))
            .await
            .expect("unscoped claim")
            .expect("cluster path may still take the foreign spawn");
    assert_eq!(unscoped.id, foreign.id);
    assert_eq!(unscoped.workspace_id, ws_b);
    assert_eq!(unscoped.lease_owner, Some(host_a));
}

#[tokio::test]
async fn dead_for_session_voids_ready_and_leased_spawn_claims() {
    let pool = connect_and_migrate().await;
    let (_user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let host_id = Uuid::now_v7();
    let session_id = Uuid::now_v7();
    let other_session = Uuid::now_v7();

    let ready = NewWorkClaim {
        id: Uuid::now_v7(),
        workspace_id,
        kind: "spawn_session".into(),
        payload: json!({ "host_id": host_id, "session_id": session_id }),
        max_attempts: 3,
        available_at: Utc::now(),
    };
    let leased = NewWorkClaim {
        id: Uuid::now_v7(),
        workspace_id,
        kind: "spawn_session".into(),
        payload: json!({ "host_id": host_id, "session_id": session_id }),
        max_attempts: 3,
        available_at: Utc::now(),
    };
    let other = NewWorkClaim {
        id: Uuid::now_v7(),
        workspace_id,
        kind: "spawn_session".into(),
        payload: json!({ "host_id": host_id, "session_id": other_session }),
        max_attempts: 3,
        available_at: Utc::now(),
    };
    work_claims::enqueue(&pool, &ready)
        .await
        .expect("enqueue ready");
    work_claims::enqueue(&pool, &leased)
        .await
        .expect("enqueue leased");
    work_claims::enqueue(&pool, &other)
        .await
        .expect("enqueue other");

    let took =
        work_claims::claim_next_spawn_for_host(&pool, workspace_id, host_id, Duration::minutes(2))
            .await
            .expect("claim")
            .expect("leased one");
    assert!(took.id == ready.id || took.id == leased.id);

    let n = work_claims::dead_for_session(&pool, session_id)
        .await
        .expect("dead_for_session");
    assert_eq!(n, 2, "ready and leased claims for the session must die");

    let leftover =
        work_claims::claim_next_spawn_for_host(&pool, workspace_id, host_id, Duration::minutes(2))
            .await
            .expect("claim leftover")
            .expect("other session must still be claimable");
    assert_eq!(leftover.id, other.id);

    let dead_ready = work_claims::get(&pool, ready.id).await.expect("get ready");
    let dead_leased = work_claims::get(&pool, leased.id)
        .await
        .expect("get leased");
    assert_eq!(dead_ready.expect("ready row").status, "dead");
    assert_eq!(dead_leased.expect("leased row").status, "dead");
}

#[tokio::test]
async fn complete_owned_rejects_wrong_host() {
    let pool = connect_and_migrate().await;
    let (_user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let host_a = Uuid::now_v7();
    let host_b = Uuid::now_v7();

    let claim = spawn_claim(workspace_id, host_a, "spawn_session");
    work_claims::enqueue(&pool, &claim).await.expect("enqueue");
    let leased =
        work_claims::claim_next_spawn_for_host(&pool, workspace_id, host_a, Duration::minutes(2))
            .await
            .expect("claim")
            .expect("leased");
    let token = leased.lease_token.expect("lease_token");

    let stolen = work_claims::complete_owned(&pool, leased.id, token, host_b, workspace_id)
        .await
        .expect("complete_owned wrong host");
    assert!(!stolen, "wrong host_id must not complete the lease");

    let other_ws = seed_user_with_workspace(&pool).await.1;
    let cross = work_claims::complete_owned(&pool, leased.id, token, host_a, other_ws)
        .await
        .expect("complete_owned wrong workspace");
    assert!(!cross, "wrong workspace_id must not complete the lease");

    let failed = work_claims::fail_owned(
        &pool,
        leased.id,
        token,
        host_b,
        workspace_id,
        "not yours",
        None,
    )
    .await
    .expect("fail_owned wrong host");
    assert!(!failed, "wrong host_id must not fail the lease");

    let ok = work_claims::complete_owned(&pool, leased.id, token, host_a, workspace_id)
        .await
        .expect("complete_owned owner");
    assert!(ok, "owning host must complete the lease");
}

#[tokio::test]
async fn complete_owned_uses_claim_workspace_not_host_home() {
    let pool = connect_and_migrate().await;
    let job_ws = seed_user_with_workspace(&pool).await.1;
    let host_id = Uuid::now_v7();
    let claim = spawn_claim(job_ws, host_id, "spawn_session");
    work_claims::enqueue(&pool, &claim).await.expect("enqueue");
    let leased = work_claims::claim_next_spawn_for_host(
        &pool,
        Uuid::now_v7(),
        host_id,
        Duration::minutes(2),
    )
    .await
    .expect("claim")
    .expect("cluster host leases by host_id");
    assert_eq!(leased.workspace_id, job_ws);
    let token = leased.lease_token.expect("lease_token");
    let ok = work_claims::complete_owned(&pool, leased.id, token, host_id, leased.workspace_id)
        .await
        .expect("complete");
    assert!(ok, "complete with claim.workspace_id must succeed");
}

/// Catalog: pair token stored hashed; list/GET never return plaintext after create.
/// Repo insert is hash-only. A plaintext column must not exist or stay null.
#[tokio::test]
async fn pair_token_hashed_at_rest_list_get_never_return_plaintext() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "hashed-rest", expires).await;
    assert!(
        token.starts_with(PAIR_TOKEN_PREFIX),
        "create must mint flp_ plaintext for the caller, got {token}"
    );

    assert_pair_token_plaintext_column_absent_or_null(&pool, host.id).await;
    assert_pair_token_hashed_at_rest(&pool, host.id, &token).await;
    let (stored, unused) = pair_token_hash_and_used_at(&pool, host.id, &token).await;
    assert!(unused.is_none(), "fresh pair token must be unused");
    assert!(
        worker_hosts::verify_token(&token, &stored),
        "stored token_hash must verify the minted plaintext"
    );
    assert_repo_host_omits_pair_plaintext(&host, &[&token]);
    assert_repo_reads_omit_pair_plaintext(&pool, workspace_id, host.id, &[&token]).await;

    let new_token = generate_pair_token();
    let replaced =
        worker_hosts::replace_pair_token(&pool, host.id, &hash_token(&new_token), expires)
            .await
            .expect("replace while pending");
    assert_pair_token_plaintext_column_absent_or_null(&pool, host.id).await;
    assert_pair_token_hashed_at_rest(&pool, host.id, &token).await;
    assert_pair_token_hashed_at_rest(&pool, host.id, &new_token).await;
    assert_repo_host_omits_pair_plaintext(&replaced, &[&token, &new_token]);
    assert_repo_reads_omit_pair_plaintext(&pool, workspace_id, host.id, &[&token, &new_token])
        .await;

    // Control: GET/list did not break pairing — the unused hash still consumes.
    let paired = pair_host(&pool, &new_token, "hashed-rest-box").await;
    assert_eq!(paired.host.id, host.id);
    assert_eq!(paired.host.status, "online");
    assert_repo_host_omits_pair_plaintext(&paired.host, &[&token, &new_token]);
    assert_repo_reads_omit_pair_plaintext(&pool, workspace_id, host.id, &[&token, &new_token])
        .await;
    assert_pair_token_hashed_at_rest(&pool, host.id, &token).await;
    assert_pair_token_hashed_at_rest(&pool, host.id, &new_token).await;
}

#[tokio::test]
async fn replace_pair_token_only_while_pending_and_invalidates_old() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, old_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "rotate", expires).await;

    let new_token = generate_pair_token();
    worker_hosts::replace_pair_token(&pool, host.id, &hash_token(&new_token), expires)
        .await
        .expect("replace while pending");

    let err = worker_hosts::consume_pair_token(
        &pool,
        &old_token,
        "box",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect_err("old pair token must die after rotate");
    match err {
        DbError::Conflict(_) | DbError::NotFound(_) => {}
        other => panic!("expected used/invalid old token, got {other:?}"),
    }

    let paired = pair_host(&pool, &new_token, "box").await;
    assert_eq!(paired.host.id, host.id);
    assert_eq!(paired.host.status, "online");

    let again = worker_hosts::replace_pair_token(
        &pool,
        host.id,
        &hash_token(&generate_pair_token()),
        expires,
    )
    .await
    .expect_err("online host must not get a new pair token");
    match again {
        DbError::Conflict(msg) => assert!(
            msg.contains("pending"),
            "conflict should say pending-only: {msg}"
        ),
        other => panic!("expected Conflict after pair, got {other:?}"),
    }
}

#[tokio::test]
async fn stored_online_with_stale_last_seen_serializes_offline() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "stale-seen", expires).await;
    pair_host(&pool, &token, "stale-box").await;

    sqlx::query(
        "UPDATE worker_hosts SET last_seen_at = now() - interval '2 minutes' WHERE id = $1",
    )
    .bind(host.id)
    .execute(&pool)
    .await
    .expect("age last_seen_at");

    let row = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(row.status, "online", "stored status stays online");
    assert_eq!(
        row.as_public(Utc::now()).status,
        "offline",
        "API/UI must report offline after 90s without a heartbeat"
    );
    assert!(
        !worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
            .await
            .expect("assignable")
            .iter()
            .any(|h| h.id == host.id),
        "stale host must not be scheduled"
    );
}

/// `install_model` queues `install_request` + `install_requested_at`; get() shows the spec.
#[tokio::test]
async fn patch_install_model_persists_install_request_on_paired_host() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("instreq");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let spec = json!({ "provider": "ollama", "spec": "llama3" });
    let before = Utc::now();
    let patched = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            install_model: Some(spec.clone()),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("patch install_model");
    assert_eq!(patched.install_request.as_ref(), Some(&spec));
    let requested_at = patched
        .install_requested_at
        .expect("install_requested_at must be set");
    assert!(
        requested_at >= before,
        "install_requested_at must be at or after the patch"
    );

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(loaded.install_request.as_ref(), Some(&spec));
    assert_eq!(
        loaded
            .install_request
            .as_ref()
            .and_then(|v| v.get("spec"))
            .and_then(|v| v.as_str()),
        Some("llama3"),
        "queued spec must be visible on get()"
    );
    assert_eq!(loaded.install_requested_at, Some(requested_at));
}

/// Mismatched ack spec must not clear a queued install (`llama3` vs `other-spec`).
#[tokio::test]
async fn clear_install_request_if_other_spec_does_not_clear() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("inst-mismatch");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let spec = json!({ "provider": "ollama", "spec": "llama3" });
    worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            install_model: Some(spec.clone()),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("queue install_request");

    let err = worker_hosts::clear_install_request_if(&pool, host.id, Some("other-spec"))
        .await
        .expect_err("mismatch must not clear the queued spec");
    match err {
        DbError::NotFound(msg) => {
            assert!(
                msg.contains("not found"),
                "mismatch currently returns NotFound: {msg}"
            );
        }
        other => panic!("expected NotFound on spec mismatch, got {other:?}"),
    }

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(
        loaded.install_request.as_ref(),
        Some(&spec),
        "queued install_request must survive a mismatched ack"
    );
    assert!(
        loaded.install_requested_at.is_some(),
        "install_requested_at must stay set after a mismatch"
    );
}

/// Matching ack spec clears both `install_request` and `install_requested_at`.
#[tokio::test]
async fn clear_install_request_if_matching_spec_clears() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("inst-match");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            install_model: Some(json!({ "provider": "ollama", "spec": "llama3" })),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("queue install_request");

    let cleared = worker_hosts::clear_install_request_if(&pool, host.id, Some("llama3"))
        .await
        .expect("matching spec must clear");
    assert!(
        cleared.install_request.is_none(),
        "matching ack must drop install_request"
    );
    assert!(
        cleared.install_requested_at.is_none(),
        "matching ack must drop install_requested_at"
    );

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert!(loaded.install_request.is_none());
    assert!(loaded.install_requested_at.is_none());
}

/// Null keys drop on merge; settings_gen bumps when runtime_settings change.
#[tokio::test]
async fn patch_runtime_settings_merges_and_null_drops_key() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("rt-settings");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let before = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    let gen_before = before.settings_gen;
    assert_eq!(before.runtime_settings, json!({}));

    let first = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            runtime_settings: Some(json!({ "harness": "grok" })),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("patch harness");
    assert_eq!(first.runtime_settings, json!({ "harness": "grok" }));

    let second = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            runtime_settings: Some(json!({ "effort": "high", "harness": null })),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("patch merge + null");
    assert_eq!(
        second.runtime_settings,
        json!({ "effort": "high" }),
        "effort stays; harness key must be gone after null merge"
    );
    assert!(
        second.runtime_settings.get("harness").is_none(),
        "null incoming value must remove the key"
    );
    assert!(
        second.settings_gen > gen_before,
        "runtime_settings patches must bump settings_gen ({gen} vs {gen_before})",
        gen = second.settings_gen
    );

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(loaded.runtime_settings, json!({ "effort": "high" }));
    assert_eq!(loaded.settings_gen, second.settings_gen);
}

/// Heartbeat `runtime_reported` is informational and must not ack pending settings.
#[tokio::test]
async fn set_runtime_reported_does_not_change_settings_ack_gen() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("rt-reported");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let patched = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            allow_host: Some(false),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("bump settings_gen");
    assert!(patched.settings_gen > 0);
    let ack_before = patched.settings_ack_gen;
    assert_eq!(ack_before, 0);
    assert!(
        patched.settings_pending(),
        "settings patch must leave apply pending"
    );
    let settings_gen = patched.settings_gen;

    worker_hosts::set_runtime_reported(&pool, host.id, json!({ "ok": true }))
        .await
        .expect("set_runtime_reported");

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(
        loaded.settings_ack_gen, ack_before,
        "runtime_reported must not change settings_ack_gen"
    );
    assert_eq!(loaded.settings_gen, settings_gen);
    assert!(
        loaded.settings_pending(),
        "pending apply must stay pending after runtime_reported"
    );
    assert_eq!(loaded.runtime_reported, json!({ "ok": true }));
}

/// PATCH 0 / 9999 / negatives must clamp into 1–256 or fail closed (nothing out of range stored).
#[tokio::test]
async fn patch_max_concurrent_0_and_9999_clamp_or_reject() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("maxc-patch");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let before = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(before.max_concurrent, 4, "create default is 4");
    assert!((1..=256).contains(&before.max_concurrent));

    for (raw, want_if_clamped) in [(0, 1), (-5, 1), (9999, 256), (512, 256)] {
        let result = worker_hosts::patch_cp_settings(
            &pool,
            host.id,
            &HostCpSettings {
                max_concurrent: Some(raw),
                ..HostCpSettings::default()
            },
        )
        .await;
        match result {
            Ok(patched) => {
                assert_eq!(
                    patched.max_concurrent, want_if_clamped,
                    "PATCH max_concurrent={raw} must clamp to {want_if_clamped}"
                );
                assert!(
                    (1..=256).contains(&patched.max_concurrent),
                    "clamped value out of range: {}",
                    patched.max_concurrent
                );
                let stored = worker_hosts::get(&pool, host.id)
                    .await
                    .expect("get after clamp")
                    .expect("host");
                assert_eq!(
                    stored.max_concurrent, want_if_clamped,
                    "stored max_concurrent after raw {raw}"
                );
            }
            Err(err) => {
                let stored = worker_hosts::get(&pool, host.id)
                    .await
                    .expect("get after reject")
                    .expect("host");
                assert_ne!(
                    stored.max_concurrent, raw,
                    "rejected PATCH {raw} must not persist the out-of-range value"
                );
                assert!(
                    (1..=256).contains(&stored.max_concurrent),
                    "stored max_concurrent after reject({raw}): {}",
                    stored.max_concurrent
                );
                match err {
                    DbError::Conflict(msg) => assert!(
                        msg.contains("max_concurrent") || msg.contains("1") || msg.contains("256"),
                        "conflict should name the range: {msg}"
                    ),
                    DbError::Sqlx(sqlx::Error::Database(db)) => {
                        let code = db.code().map(|c| c.to_string()).unwrap_or_default();
                        assert!(
                            code == "23514" || db.message().contains("max_concurrent"),
                            "expected check violation, got code={code} msg={}",
                            db.message()
                        );
                    }
                    other => panic!(
                        "PATCH max_concurrent={raw} must clamp or fail closed, got {other:?}"
                    ),
                }
            }
        }
    }

    for keep in [1, 8, 256] {
        let patched = worker_hosts::patch_cp_settings(
            &pool,
            host.id,
            &HostCpSettings {
                max_concurrent: Some(keep),
                ..HostCpSettings::default()
            },
        )
        .await
        .unwrap_or_else(|e| panic!("in-range PATCH {keep} must persist: {e}"));
        assert_eq!(patched.max_concurrent, keep, "in-range {keep}");
        let stored = worker_hosts::get(&pool, host.id)
            .await
            .expect("get in-range")
            .expect("host");
        assert_eq!(stored.max_concurrent, keep, "stored in-range {keep}");
    }
}

/// Effective cap is min(clamped plane, clamped advertised). Advertised 0 is not unlimited.
#[tokio::test]
async fn effective_max_concurrent_is_min_of_plane_and_advertised() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("maxc-eff");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let plane8 = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            max_concurrent: Some(8),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("set plane 8");
    assert_eq!(plane8.max_concurrent, 8);
    assert_eq!(
        worker_hosts::effective_max_concurrent(&plane8),
        8,
        "no advertised report → plane cap"
    );
    assert!(!worker_hosts::at_job_cap(&plane8), "0 jobs is under cap");

    let adv2 = worker_hosts::heartbeat(
        &pool,
        host.id,
        true,
        false,
        None,
        None,
        None,
        1,
        Some(2),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("advertise 2");
    assert_eq!(adv2.max_concurrent, 8);
    assert_eq!(adv2.advertised_max_concurrent, Some(2));
    assert_eq!(
        worker_hosts::effective_max_concurrent(&adv2),
        2,
        "advertised 2 must win over plane 8"
    );
    assert!(
        !worker_hosts::at_job_cap(&adv2),
        "1 < min(8,2) is under cap"
    );

    let at_adv = worker_hosts::heartbeat(
        &pool,
        host.id,
        true,
        false,
        None,
        None,
        None,
        2,
        Some(2),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("fill advertised cap");
    assert_eq!(worker_hosts::effective_max_concurrent(&at_adv), 2);
    assert!(
        worker_hosts::at_job_cap(&at_adv),
        "current_jobs == advertised min must be at cap"
    );

    let plane2 = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            max_concurrent: Some(2),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("set plane 2");
    let adv8 = worker_hosts::heartbeat(
        &pool,
        host.id,
        true,
        false,
        None,
        None,
        None,
        2,
        Some(8),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("advertise 8");
    assert_eq!(plane2.max_concurrent, 2);
    assert_eq!(adv8.advertised_max_concurrent, Some(8));
    assert_eq!(
        worker_hosts::effective_max_concurrent(&adv8),
        2,
        "plane 2 must win over advertised 8"
    );
    assert!(worker_hosts::at_job_cap(&adv8));

    let zero_adv = worker_hosts::heartbeat(
        &pool,
        host.id,
        true,
        false,
        None,
        None,
        None,
        1,
        Some(0),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("advertise 0");
    assert_eq!(
        zero_adv.advertised_max_concurrent,
        Some(1),
        "heartbeat must clamp advertised 0 → 1 (not unlimited)"
    );
    assert_eq!(
        worker_hosts::effective_max_concurrent(&zero_adv),
        1,
        "0 advertised is a cap of 1"
    );
    assert!(
        worker_hosts::at_job_cap(&zero_adv),
        "1 job against advertised-0 (clamped 1) is at cap"
    );

    let huge_adv = worker_hosts::heartbeat(
        &pool,
        host.id,
        true,
        false,
        None,
        None,
        None,
        2,
        Some(9999),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("advertise 9999");
    assert_eq!(huge_adv.advertised_max_concurrent, Some(256));
    assert_eq!(
        worker_hosts::effective_max_concurrent(&huge_adv),
        2,
        "clamped advertised 256 still min'd with plane 2"
    );
}

/// Hosts at min(plane, advertised) must drop out of list_assignable; under-cap peers stay.
#[tokio::test]
async fn list_assignable_skips_host_at_max_concurrent_cap() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);

    async fn pair_named(
        pool: &fleet_db::PgPool,
        workspace_id: Uuid,
        user_id: Uuid,
        expires: chrono::DateTime<Utc>,
        name: &str,
    ) -> Uuid {
        let (host, token) =
            create_pending_with_token(pool, workspace_id, user_id, name, expires).await;
        pair_host(pool, &token, &format!("{name}-box")).await;
        host.id
    }

    let under = pair_named(&pool, workspace_id, user_id, expires, "under-cap").await;
    let at_adv = pair_named(&pool, workspace_id, user_id, expires, "at-adv-cap").await;
    let at_plane = pair_named(&pool, workspace_id, user_id, expires, "at-plane-cap").await;
    let plane_wins = pair_named(&pool, workspace_id, user_id, expires, "plane-wins").await;
    let zero_adv = pair_named(&pool, workspace_id, user_id, expires, "zero-adv").await;

    worker_hosts::patch_cp_settings(
        &pool,
        under,
        &HostCpSettings {
            max_concurrent: Some(8),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("under plane 8");
    worker_hosts::heartbeat(
        &pool,
        under,
        true,
        false,
        None,
        None,
        None,
        1,
        Some(2),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("under: 1 of min(8,2)");

    worker_hosts::patch_cp_settings(
        &pool,
        at_adv,
        &HostCpSettings {
            max_concurrent: Some(8),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("at_adv plane 8");
    worker_hosts::heartbeat(
        &pool,
        at_adv,
        true,
        false,
        None,
        None,
        None,
        2,
        Some(2),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("at_adv: 2 of min(8,2)");

    worker_hosts::patch_cp_settings(
        &pool,
        at_plane,
        &HostCpSettings {
            max_concurrent: Some(1),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("at_plane plane 1");
    worker_hosts::heartbeat(
        &pool, at_plane, true, false, None, None, None, 1, None, None, None, None, None,
    )
    .await
    .expect("at_plane: 1 of 1");

    worker_hosts::patch_cp_settings(
        &pool,
        plane_wins,
        &HostCpSettings {
            max_concurrent: Some(2),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("plane_wins plane 2");
    worker_hosts::heartbeat(
        &pool,
        plane_wins,
        true,
        false,
        None,
        None,
        None,
        2,
        Some(8),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("plane_wins: 2 of min(2,8)");

    worker_hosts::patch_cp_settings(
        &pool,
        zero_adv,
        &HostCpSettings {
            max_concurrent: Some(8),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("zero_adv plane 8");
    worker_hosts::heartbeat(
        &pool,
        zero_adv,
        true,
        false,
        None,
        None,
        None,
        1,
        Some(0),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("zero_adv: advertised 0 clamps to 1");

    let loaded_under = worker_hosts::get(&pool, under)
        .await
        .expect("get under")
        .expect("under");
    let loaded_adv = worker_hosts::get(&pool, at_adv)
        .await
        .expect("get at_adv")
        .expect("at_adv");
    let loaded_plane = worker_hosts::get(&pool, at_plane)
        .await
        .expect("get at_plane")
        .expect("at_plane");
    let loaded_wins = worker_hosts::get(&pool, plane_wins)
        .await
        .expect("get plane_wins")
        .expect("plane_wins");
    let loaded_zero = worker_hosts::get(&pool, zero_adv)
        .await
        .expect("get zero_adv")
        .expect("zero_adv");

    assert_eq!(worker_hosts::effective_max_concurrent(&loaded_under), 2);
    assert!(!worker_hosts::at_job_cap(&loaded_under));
    assert_eq!(worker_hosts::effective_max_concurrent(&loaded_adv), 2);
    assert!(worker_hosts::at_job_cap(&loaded_adv));
    assert_eq!(worker_hosts::effective_max_concurrent(&loaded_plane), 1);
    assert!(worker_hosts::at_job_cap(&loaded_plane));
    assert_eq!(worker_hosts::effective_max_concurrent(&loaded_wins), 2);
    assert!(worker_hosts::at_job_cap(&loaded_wins));
    assert_eq!(loaded_zero.advertised_max_concurrent, Some(1));
    assert_eq!(worker_hosts::effective_max_concurrent(&loaded_zero), 1);
    assert!(worker_hosts::at_job_cap(&loaded_zero));

    let assignable = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
        .await
        .expect("list_assignable");
    let ids = assignable_ids_in_workspace(&assignable, workspace_id);
    assert!(
        ids.contains(&under),
        "under-cap host must stay assignable: {ids:?}"
    );
    assert!(
        !ids.contains(&at_adv),
        "advertised-cap host must be skipped: {ids:?}"
    );
    assert!(
        !ids.contains(&at_plane),
        "plane-cap host (no advertised) must be skipped: {ids:?}"
    );
    assert!(
        !ids.contains(&plane_wins),
        "min(plane=2, advertised=8) at 2 must be skipped: {ids:?}"
    );
    assert!(
        !ids.contains(&zero_adv),
        "advertised 0 is not unlimited; 1 job must skip: {ids:?}"
    );
    assert_eq!(
        ids,
        vec![under],
        "only the under-cap native must be assignable: {ids:?}"
    );
}
