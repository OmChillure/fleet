//! Fail-closed Postgres tests for paired worker hosts.
//! Requires DATABASE_URL — never skips.

mod common;

use chrono::{Duration, Utc};
use fleet_db::error::DbError;
use fleet_db::models::{HostCpSettings, NewWorkClaim, NewWorkerHost};
use fleet_db::repos::users::NewUser;
use fleet_db::repos::worker_hosts::{
    self, generate_pair_token, hash_token, PAIR_TOKEN_PREFIX,
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
                'pair_token_plaintext', 'pair_token_plain'
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

/// HTTP GET /worker-hosts is list_for_workspace: this workspace's rows only.
#[tokio::test]
async fn create_pending_then_list_for_workspace() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let other_ws = seed_user_with_workspace(&pool).await.1;
    let expires = Utc::now() + Duration::hours(1);

    let (a, _) = create_pending_with_token(&pool, workspace_id, user_id, "studio", expires).await;
    let (b, _) = create_pending_with_token(&pool, workspace_id, user_id, "lab", expires).await;
    let (c, _) = create_pending_with_token(&pool, other_ws, user_id, "other", expires).await;

    let listed = worker_hosts::list_for_workspace(&pool, workspace_id)
        .await
        .expect("list");
    let ids: Vec<Uuid> = listed.iter().map(|h| h.id).collect();
    assert!(ids.contains(&a.id), "missing studio host");
    assert!(ids.contains(&b.id), "missing lab host");
    assert!(
        !ids.contains(&c.id),
        "list_for_workspace must omit the other workspace's host"
    );
}

/// load_visible_host: get_in_workspace on a foreign workspace is None.
#[tokio::test]
async fn get_in_workspace_other_workspace_id_is_none() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let other_ws = seed_user_with_workspace(&pool).await.1;
    let expires = Utc::now() + Duration::hours(1);
    let (host, _) =
        create_pending_with_token(&pool, workspace_id, user_id, "studio", expires).await;

    let scoped = worker_hosts::get_in_workspace(&pool, workspace_id, host.id)
        .await
        .expect("get_in_workspace");
    assert!(scoped.is_some());
    let cross = worker_hosts::get_in_workspace(&pool, other_ws, host.id)
        .await
        .expect("cross-workspace get");
    assert!(cross.is_none());
}

/// HTTP GET /worker-hosts must not merge other workspaces' cluster hosts
/// (list_assignable is the unscope twin).
#[tokio::test]
async fn list_for_workspace_omits_other_workspace_cluster_host() {
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

    let from_b = worker_hosts::list_for_workspace(&pool, ws_b)
        .await
        .expect("list_for_workspace B");
    assert!(
        !from_b.iter().any(|h| h.id == host.id),
        "cluster host home={ws_a} must not appear in list_for_workspace({ws_b}): {:?}",
        from_b.iter().map(|h| h.id).collect::<Vec<_>>()
    );
}

/// GET /worker-hosts card order is created_at ASC, id ASC.
#[tokio::test]
async fn list_for_workspace_orders_by_created_at_asc_then_id() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (older, _) =
        create_pending_with_token(&pool, workspace_id, user_id, "older", expires).await;
    let (newer, _) =
        create_pending_with_token(&pool, workspace_id, user_id, "newer", expires).await;

    let listed = worker_hosts::list_for_workspace(&pool, workspace_id)
        .await
        .expect("list");
    let ids: Vec<Uuid> = listed.iter().map(|h| h.id).collect();
    assert_eq!(ids, vec![older.id, newer.id]);
}

/// get / get_in_workspace miss is None (load_visible_host → 404).
#[tokio::test]
async fn get_and_get_in_workspace_unknown_id_are_none() {
    let pool = connect_and_migrate().await;
    let (_user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let missing = Uuid::now_v7();

    let got = worker_hosts::get(&pool, missing).await.expect("get");
    assert!(got.is_none());
    let scoped = worker_hosts::get_in_workspace(&pool, workspace_id, missing)
        .await
        .expect("get_in_workspace");
    assert!(scoped.is_none());
}

/// Second consume of a used flp_ is Conflict("already used") and must not
/// rotate machine_token_hash, rewrite used_at, or demote the leftover online row.
#[tokio::test]
async fn pair_token_works_once_second_consume_fails() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "once", expires).await;
    let paired = pair_host(&pool, &token, "box-1").await;
    let (_, used) = pair_token_hash_and_used_at(&pool, host.id, &token).await;

    let err =
        worker_hosts::consume_pair_token(&pool, &token, "box-1", "linux-x86_64", true, false, None)
            .await
            .expect_err("second consume must fail");
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

/// generate_pair_token that was never inserted is NotFound (HTTP 401) and
/// must not mint a host row.
#[tokio::test]
async fn consume_pair_token_unknown_hash_is_not_found() {
    let pool = connect_and_migrate().await;
    let (_user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let token = generate_pair_token();

    let err = worker_hosts::consume_pair_token(
        &pool,
        &token,
        "unknown-hash-box",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect_err("never-inserted pair token must miss");
    match err {
        DbError::NotFound(_) => {}
        other => panic!("expected NotFound on unknown pair-token hash, got {other:?}"),
    }

    let listed = worker_hosts::list_for_workspace(&pool, workspace_id)
        .await
        .expect("list");
    assert!(
        listed.is_empty(),
        "unknown hash must not insert a host: {:?}",
        listed.iter().map(|h| h.id).collect::<Vec<_>>()
    );
}

/// Unused token on an already-online host hits status!=pending (after used/expiry)
/// and must not set used_at or rotate machine_token_hash.
#[tokio::test]
async fn consume_pair_token_unused_token_on_online_host_is_not_pending() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "online-unused", expires).await;
    let paired = pair_host(&pool, &token, "box-online").await;

    sqlx::query("UPDATE worker_host_pair_tokens SET used_at = NULL WHERE host_id = $1")
        .bind(host.id)
        .execute(&pool)
        .await
        .expect("clear used_at, leave status online");

    let err = worker_hosts::consume_pair_token(
        &pool,
        &token,
        "box-online",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect_err("unused token on online host must fail");
    assert_pair_conflict(err, "pending");

    let (_, used_at) = pair_token_hash_and_used_at(&pool, host.id, &token).await;
    assert!(
        used_at.is_none(),
        "failed not-pending consume must not set used_at"
    );
    let still = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host remains");
    assert_eq!(still.machine_token_hash, paired.host.machine_token_hash);
}

/// create_pending name that is only whitespace falls back to the consume hostname.
#[tokio::test]
async fn consume_pair_token_empty_name_uses_hostname() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "   ", expires).await;
    assert_eq!(host.name, "   ");

    let paired = pair_host(&pool, &token, "studio-box").await;
    assert_eq!(paired.host.id, host.id);
    assert_eq!(paired.host.name, "studio-box");
}

/// Leftover unused hashes on the same host are marked used when one token pairs.
#[tokio::test]
async fn pair_token_consume_once_and_expired_must_fail() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "leftover-kill", expires).await;
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
    .bind(host.id)
    .bind(workspace_id)
    .bind(hash_token(&leftover))
    .bind(expires)
    .execute(&pool)
    .await
    .expect("insert leftover unused pair token");

    pair_host(&pool, &token, "box-leftover-kill").await;

    let (_, leftover_used) = pair_token_hash_and_used_at(&pool, host.id, &leftover).await;
    assert!(
        leftover_used.is_some(),
        "leftover unused tokens must be marked used when the host leaves pending"
    );
}

/// used_at, not host.status, is the consume-once gate.
#[tokio::test]
async fn used_pair_token_stays_dead_after_pending_status_reset() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "reset-pending", expires).await;
    pair_host(&pool, &token, "box-reset").await;
    sqlx::query("UPDATE worker_hosts SET status = 'pending' WHERE id = $1")
        .bind(host.id)
        .execute(&pool)
        .await
        .expect("reset host to pending");

    let err = worker_hosts::consume_pair_token(
        &pool,
        &token,
        "box-reset-again",
        "linux-aarch64",
        true,
        true,
        None,
    )
    .await
    .expect_err("used pair token must stay dead after status reset");
    assert_pair_conflict(err, "used");

    let still = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host remains");
    assert_eq!(
        still.status, "pending",
        "failed reuse must not promote the host"
    );
}

/// used_at blocks consume even when the host never left pending.
#[tokio::test]
async fn consume_pair_token_preused_while_pending_is_used() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "preused", expires).await;
    sqlx::query("UPDATE worker_host_pair_tokens SET used_at = now() WHERE host_id = $1")
        .bind(host.id)
        .execute(&pool)
        .await
        .expect("mark used while still pending");

    let err = worker_hosts::consume_pair_token(
        &pool,
        &token,
        "box-preused",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect_err("used_at must block consume even while host is pending");
    assert_pair_conflict(err, "used");

    let still = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host remains");
    assert_eq!(still.status, "pending");
    assert!(still.machine_token_hash.is_none());
    assert!(still.paired_at.is_none());
}

/// expires_at == now is already expired (consume uses <=).
#[tokio::test]
async fn consume_pair_token_expires_at_equal_now_is_expired() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "boundary", Utc::now()).await;

    let err = worker_hosts::consume_pair_token(
        &pool,
        &token,
        "box-boundary",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect_err("expires_at == now must fail as expired");
    assert_pair_conflict(err, "expired");

    let still = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host remains");
    assert_eq!(still.status, "pending");
    assert!(still.machine_token_hash.is_none());
    let (_, used) = pair_token_hash_and_used_at(&pool, host.id, &token).await;
    assert!(used.is_none(), "expires_at == now must not set used_at");
}

/// Consume reads expires_at, not created_at + 1h.
#[tokio::test]
async fn consume_pair_token_ignores_created_at_when_expires_at_is_future() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "window", expires).await;
    sqlx::query(
        "UPDATE worker_host_pair_tokens SET created_at = now() - interval '2 hours' WHERE host_id = $1",
    )
    .bind(host.id)
    .execute(&pool)
    .await
    .expect("age created_at without touching expires_at");

    let paired = worker_hosts::consume_pair_token(
        &pool,
        &token,
        "box-window",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect("future expires_at must consume even if created_at is 2h old");
    assert_eq!(paired.host.id, host.id);
    assert_eq!(paired.host.status, "online");
}

/// Parallel consume-once: FOR UPDATE admits exactly one winner.
#[tokio::test]
async fn consume_pair_token_parallel_for_update_admits_one_winner() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "race", expires).await;

    let (first, second) = tokio::join!(
        worker_hosts::consume_pair_token(
            &pool,
            &token,
            "box-race-a",
            "linux-x86_64",
            true,
            false,
            None,
        ),
        worker_hosts::consume_pair_token(
            &pool,
            &token,
            "box-race-b",
            "linux-x86_64",
            true,
            false,
            None,
        ),
    );
    let loser = match (first, second) {
        (Ok(_), Err(err)) | (Err(err), Ok(_)) => err,
        other => panic!("exactly one parallel consume must succeed: {other:?}"),
    };
    assert_pair_conflict(loser, "used");

    let raced = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host remains");
    assert_eq!(raced.status, "online");
    let (_, used) = pair_token_hash_and_used_at(&pool, host.id, &token).await;
    assert!(used.is_some(), "winning consume must set used_at");
}

/// leftover UPDATE is WHERE host_id = $1; another host's unused token stays live.
#[tokio::test]
async fn leftover_pair_token_on_other_host_survives_consume() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (_host_a, token_a) =
        create_pending_with_token(&pool, workspace_id, user_id, "host-a", expires).await;
    let (host_b, token_b) =
        create_pending_with_token(&pool, workspace_id, user_id, "host-b", expires).await;

    pair_host(&pool, &token_a, "box-a").await;

    let (_, b_used) = pair_token_hash_and_used_at(&pool, host_b.id, &token_b).await;
    assert!(
        b_used.is_none(),
        "unused token on another host must survive leftover UPDATE"
    );
    let paired_b = pair_host(&pool, &token_b, "box-b").await;
    assert_eq!(paired_b.host.id, host_b.id);
    assert_eq!(paired_b.host.status, "online");
}

/// After revoke, consume with the same machine.id revives that row on a new
/// hostname/platform, keeps operator CP columns, rotates the token, and
/// clears last_error. Leftover pending must not steal machine.id.
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
    let old_token = paired.machine_token.clone();

    worker_hosts::patch_cp_settings(
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

    let beat = worker_hosts::heartbeat(
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
    assert_eq!(beat.last_error.as_deref(), Some("disk full"));

    worker_hosts::revoke(&pool, first.id).await.expect("revoke");

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
    assert_eq!(again.host.machine_id, Some(mid));
    assert_eq!(again.host.name, "studio-kept");
    assert_eq!(again.host.labels, vec!["gpu".to_string()]);
    assert_eq!(again.host.max_concurrent, 7);
    assert_eq!(again.host.settings_gen, 1);
    assert_eq!(again.host.hostname.as_deref(), Some("box-renamed-os"));
    assert_eq!(again.host.platform.as_deref(), Some("linux-aarch64"));
    assert!(
        again.host.last_error.is_none(),
        "reclaim clears last_error from the prior life"
    );
    assert_ne!(again.machine_token, old_token);

    let leftover = worker_hosts::get(&pool, second.id)
        .await
        .expect("get leftover")
        .expect("pending leftover exists");
    assert!(
        leftover.machine_id.is_none(),
        "leftover must not steal the durable machine.id"
    );
}

/// machine.id lookup has no status filter: re-pair while still online
/// reclaims the live row, rotates the machine token, and disables the pending.
#[tokio::test]
async fn re_pair_same_machine_id_reclaims_still_online_row() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let mid = Uuid::now_v7();

    let (first, token_a) =
        create_pending_with_token(&pool, workspace_id, user_id, "live", expires).await;
    let paired = worker_hosts::consume_pair_token(
        &pool,
        &token_a,
        "live-box",
        "linux-x86_64",
        true,
        false,
        Some(mid),
    )
    .await
    .expect("first pair");
    assert_eq!(paired.host.id, first.id);
    let old_token = paired.machine_token.clone();

    let (pending, token_b) =
        create_pending_with_token(&pool, workspace_id, user_id, "clone", expires).await;
    let again = worker_hosts::consume_pair_token(
        &pool,
        &token_b,
        "live-box-again",
        "linux-x86_64",
        true,
        false,
        Some(mid),
    )
    .await
    .expect("re-pair still-online");

    assert_eq!(again.host.id, first.id);
    assert_eq!(again.host.status, "online");
    assert_eq!(again.host.machine_id, Some(mid));
    assert_ne!(again.machine_token, old_token);

    let leftover = worker_hosts::get(&pool, pending.id)
        .await
        .expect("get leftover")
        .expect("pending leftover exists");
    assert_eq!(leftover.status, "disabled");
}

/// reclaim_existing display_name falls back to the new hostname when
/// prior.name is only whitespace.
#[tokio::test]
async fn re_pair_same_machine_id_empty_prior_name_uses_new_hostname() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let mid = Uuid::now_v7();

    let (first, token_a) =
        create_pending_with_token(&pool, workspace_id, user_id, "box", expires).await;
    let paired = worker_hosts::consume_pair_token(
        &pool,
        &token_a,
        "box-old",
        "linux-x86_64",
        true,
        false,
        Some(mid),
    )
    .await
    .expect("first pair");
    assert_eq!(paired.host.id, first.id);

    let blanked = worker_hosts::patch_cp_settings(
        &pool,
        first.id,
        &HostCpSettings {
            name: Some("   ".into()),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("blank name");
    assert_eq!(blanked.name, "   ");

    worker_hosts::revoke(&pool, first.id).await.expect("revoke");

    let (_second, token_b) =
        create_pending_with_token(&pool, workspace_id, user_id, "throwaway", expires).await;
    let again = worker_hosts::consume_pair_token(
        &pool,
        &token_b,
        "studio-renamed",
        "linux-x86_64",
        true,
        false,
        Some(mid),
    )
    .await
    .expect("re-pair");

    assert_eq!(again.host.id, first.id);
    assert_eq!(again.host.name, "studio-renamed");
}

/// reclaim UPDATE omits settings_ack_gen; an acked gen must survive revoke+re-pair.
#[tokio::test]
async fn reclaim_keeps_settings_ack_gen() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let mid = Uuid::now_v7();

    let (first, token_a) =
        create_pending_with_token(&pool, workspace_id, user_id, "ack", expires).await;
    let paired = worker_hosts::consume_pair_token(
        &pool,
        &token_a,
        "ack-box",
        "linux-x86_64",
        true,
        false,
        Some(mid),
    )
    .await
    .expect("first pair");
    assert_eq!(paired.host.id, first.id);

    worker_hosts::patch_cp_settings(
        &pool,
        first.id,
        &HostCpSettings {
            allow_host: Some(false),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("bump settings_gen");
    worker_hosts::ack_settings(&pool, first.id, 1)
        .await
        .expect("ack");

    worker_hosts::revoke(&pool, first.id).await.expect("revoke");

    let (_second, token_b) =
        create_pending_with_token(&pool, workspace_id, user_id, "throwaway", expires).await;
    let again = worker_hosts::consume_pair_token(
        &pool,
        &token_b,
        "ack-box-again",
        "linux-x86_64",
        true,
        false,
        Some(mid),
    )
    .await
    .expect("re-pair");

    assert_eq!(again.host.id, first.id);
    assert_eq!(again.host.settings_gen, 1);
    assert_eq!(again.host.settings_ack_gen, 1);
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

/// Distinct machine.id must not hostname-reclaim. Revoke first so a mid miss
/// that fell through to hostname+platform would steal the disabled row.
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

    worker_hosts::revoke(&pool, first.id).await.expect("revoke first");

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
    assert_eq!(
        b.host.id, second.id,
        "distinct machine.id must not hostname-reclaim a disabled same-hostname row"
    );
    assert_eq!(b.host.machine_id, Some(mid_b));
    assert_eq!(b.host.status, "online");
    assert_ne!(a.host.id, b.host.id);

    let still = worker_hosts::get(&pool, first.id)
        .await
        .expect("get first")
        .expect("first still exists");
    assert_eq!(still.status, "disabled");
    assert_eq!(still.machine_id, Some(mid_a));
    assert_eq!(still.hostname.as_deref(), Some("ubuntu"));
}

/// catalog:unit-apply-ack — max_concurrent-only Apply still saturating_adds settings_gen.
#[tokio::test]
async fn each_settings_apply_increments_settings_gen() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("gen-bump");
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
    .expect("prior apply");

    let bumped = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            max_concurrent: Some(2),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("max_concurrent apply");
    assert_eq!(bumped.max_concurrent, 2);
    assert_eq!(
        bumped.settings_gen, 2,
        "max_concurrent-only Apply after another field change must saturating_add"
    );
    assert_eq!(bumped.settings_ack_gen, 0);
    assert!(bumped.settings_pending());
}

/// catalog:unit-apply-ack — PATCH of the already-acked snapshot must not relight pending.
#[tokio::test]
async fn patch_cp_settings_after_ack_same_values_does_not_repend() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("gen-ack-noop");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let applied = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            allow_host: Some(false),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("apply");
    worker_hosts::ack_settings(&pool, host.id, applied.settings_gen)
        .await
        .expect("ack");

    let again = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            allow_host: Some(false),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("same snapshot");
    assert_eq!(again.settings_gen, 1);
    assert_eq!(again.settings_ack_gen, 1);
    assert!(!again.settings_pending());
    assert!(!again.allow_host);
}

/// catalog:unit-apply-ack — PATCH on a missing host is NotFound.
#[tokio::test]
async fn patch_cp_settings_missing_host_is_not_found() {
    let pool = connect_and_migrate().await;
    let missing = Uuid::now_v7();

    let err = worker_hosts::patch_cp_settings(
        &pool,
        missing,
        &HostCpSettings {
            allow_host: Some(false),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect_err("missing host");
    match err {
        DbError::NotFound(msg) => {
            assert!(
                msg.contains("not found"),
                "missing host must be NotFound: {msg}"
            );
        }
        other => panic!("expected NotFound on missing host, got {other:?}"),
    }
    assert!(
        worker_hosts::get(&pool, missing)
            .await
            .expect("get")
            .is_none(),
        "missing PATCH must not insert a host"
    );
}

/// catalog:unit-apply-ack — PATCH max_concurrent already at default 4 does not bump gen.
#[tokio::test]
async fn patch_max_concurrent_already_current_does_not_bump() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("maxc-same");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let before = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(before.max_concurrent, 4, "create default is 4");
    assert_eq!(before.settings_gen, 0);

    let patched = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            max_concurrent: Some(4),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("already-current max_concurrent");
    assert_eq!(patched.max_concurrent, 4);
    assert_eq!(patched.settings_gen, 0);
    assert!(!patched.settings_pending());
}

/// catalog:unit-apply-ack — after ack+re-apply, echoing the prior gen must not clear pending.
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
    .expect("apply");
    worker_hosts::ack_settings(&pool, host.id, 1)
        .await
        .expect("ack current");
    worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            drain: Some(true),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("re-apply");

    let _ = worker_hosts::ack_settings(&pool, host.id, 1).await;

    let after_prior = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(after_prior.settings_gen, 2);
    assert_eq!(
        after_prior.settings_ack_gen, 1,
        "prior-gen ack must not rewind or jump settings_ack_gen"
    );
    assert!(
        after_prior.settings_pending(),
        "ack of the last good gen after a new apply must leave apply pending"
    );
}

/// catalog:unit-apply-ack — a future settings_gen must not SET settings_ack_gen.
#[tokio::test]
async fn ack_future_settings_gen_does_not_advance_ack() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("gen-future");
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
    worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            paused: Some(true),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("apply gen 2");

    worker_hosts::ack_settings(&pool, host.id, 99)
        .await
        .expect_err("ack of a future gen must fail");

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(loaded.settings_gen, 2);
    assert_eq!(
        loaded.settings_ack_gen, 0,
        "future ack must not advance settings_ack_gen"
    );
    assert!(loaded.settings_pending());
}

/// catalog:unit-apply-ack — heartbeat UPDATE must not SET settings_ack_gen.
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
    .expect("apply");
    assert_eq!(first.settings_gen, 1);
    assert_eq!(first.settings_ack_gen, 0);
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
        "heartbeat must not set settings_ack_gen"
    );
    assert!(
        beat.settings_pending(),
        "apply must stay pending after heartbeat"
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
    worker_hosts::ack_settings(&pool, host.id, off.settings_gen)
        .await
        .expect("ack auto off");
    let settings_gen = off.settings_gen;

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
        requested.settings_gen, settings_gen,
        "Update now is a one-shot, not a settings bump"
    );
    assert!(
        !requested.settings_pending(),
        "request_update must not light settings_pending"
    );

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(loaded.update_requested_at, Some(at));
    assert_eq!(loaded.settings_gen, settings_gen);
    assert!(!loaded.settings_pending());
    assert!(!loaded.auto_update);
}

/// catalog:unit-request-update — second Update now must replace a planted stamp.
#[tokio::test]
async fn request_update_second_click_replaces_planted_timestamp() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "upd-plant", expires).await;
    pair_host(&pool, &token, "upd-plant-box").await;

    let planted = Utc::now() - Duration::hours(1);
    sqlx::query("UPDATE worker_hosts SET update_requested_at = $2 WHERE id = $1")
        .bind(host.id)
        .bind(planted)
        .execute(&pool)
        .await
        .expect("plant update_requested_at");
    let stored = worker_hosts::get(&pool, host.id)
        .await
        .expect("get planted")
        .expect("host")
        .update_requested_at
        .expect("planted stamp");

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
    let at = requested
        .update_requested_at
        .expect("update_requested_at must be set");
    assert!(
        at > stored,
        "second Update now must replace the planted stamp ({at} > {stored})"
    );
}

/// catalog:unit-request-update — auto_update=false is a settings apply.
#[tokio::test]
async fn patch_auto_update_false_bumps_settings_gen() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "auto-off", expires).await;
    pair_host(&pool, &token, "auto-off-box").await;

    let before = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert!(before.auto_update, "paired host defaults auto-update on");
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
    assert_eq!(off.settings_gen, settings_gen + 1);
    assert!(
        off.settings_pending(),
        "auto_update change must light settings_pending"
    );
}

/// catalog:unit-request-update — later CP PATCH must not clear Update now.
#[tokio::test]
async fn later_cp_patch_keeps_update_requested_at() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "upd-keep", expires).await;
    pair_host(&pool, &token, "upd-keep-box").await;

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
    let at = requested
        .update_requested_at
        .expect("update_requested_at must be set");

    let later = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            paused: Some(true),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("later paused");
    assert!(later.paused);
    assert_eq!(
        later.update_requested_at,
        Some(at),
        "later CP PATCH must keep update_requested_at"
    );

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(loaded.update_requested_at, Some(at));
    assert!(loaded.paused);
}

/// revoke of a never-paired pending marks unused pair tokens used;
/// consume of the original flp_ is Conflict used.
#[tokio::test]
async fn revoke_never_paired_pending_marks_pair_token_used() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "unused-revoke-burn", expires)
            .await;

    worker_hosts::revoke(&pool, host.id)
        .await
        .expect("revoke unused pending");

    let err = worker_hosts::consume_pair_token(
        &pool,
        &token,
        "unused-revoke-box",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect_err("original flp_ must not consume after unused revoke");
    assert_pair_conflict(err, "used");
}

/// Heartbeat on a disabled id is NotFound and must not advance last_seen.
/// HTTP `/me/heartbeat` 401s in require_machine before this UPDATE.
#[tokio::test]
async fn revoked_host_is_not_list_assignable_and_cannot_take_a_lease() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);

    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "revoke-hb", expires).await;
    let paired = pair_host(&pool, &token, "revoke-hb-box").await;
    let seen_before = paired.host.last_seen_at.expect("pair sets last_seen_at");

    worker_hosts::revoke(&pool, host.id).await.expect("revoke");

    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let hb_err = worker_hosts::heartbeat(
        &pool,
        host.id,
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
    let still = worker_hosts::get(&pool, host.id)
        .await
        .expect("get revoked")
        .expect("revoked row remains");
    assert_eq!(still.status, "disabled");
    assert_eq!(
        still.last_seen_at,
        Some(seen_before),
        "failed heartbeat must not advance last_seen_at"
    );
}

/// replace_pair_token is pending-only; a revoked (disabled) host is Conflict.
#[tokio::test]
async fn replace_pair_token_after_revoke_is_conflict() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "revoke-replace", expires).await;
    pair_host(&pool, &token, "revoke-replace-box").await;

    worker_hosts::revoke(&pool, host.id).await.expect("revoke");

    let err = worker_hosts::replace_pair_token(
        &pool,
        host.id,
        &hash_token("flp_must-not-replace-after-revoke"),
        Utc::now() + Duration::hours(1),
    )
    .await
    .expect_err("replace_pair_token only while pending");
    match err {
        DbError::Conflict(msg) => assert!(
            msg.contains("pending"),
            "disabled host re-pairs via a new pending, not replace_pair_token: {msg}"
        ),
        other => panic!("expected Conflict on replace_pair_token after revoke, got {other:?}"),
    }
}

/// Heartbeat SET last_seen_at = now() on a live host. Pair only checks Some;
/// revoke checks a failed beat does not move it; stored_online ages via SQL.
#[tokio::test]
async fn heartbeat_updates_last_seen_at() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "hb", expires).await;
    pair_host(&pool, &token, "box-hb").await;

    sqlx::query(
        "UPDATE worker_hosts SET last_seen_at = now() - interval '2 seconds' WHERE id = $1",
    )
    .bind(host.id)
    .execute(&pool)
    .await
    .expect("age last_seen_at");
    let first_seen = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host")
        .last_seen_at
        .expect("aged last_seen_at");

    let after = worker_hosts::heartbeat(
        &pool, host.id, true, false, None, None, None, 0, None, None, None, None, None,
    )
    .await
    .expect("heartbeat");

    let seen = after.last_seen_at.expect("heartbeat sets last_seen_at");
    assert!(
        seen > first_seen,
        "last_seen_at must advance past the aged stamp"
    );
    assert!(
        seen - first_seen >= Duration::seconds(1),
        "last_seen_at must be Postgres now(), not the planted stamp"
    );
}

/// last_error = $6 is assigned, not COALESCE. HTTP maps omitted/empty error to None.
#[tokio::test]
async fn heartbeat_omitted_last_error_clears_previous() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "hb-err", expires).await;
    pair_host(&pool, &token, "box-hb-err").await;

    let planted = worker_hosts::heartbeat(
        &pool,
        host.id,
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
    .expect("plant last_error");
    assert_eq!(planted.last_error.as_deref(), Some("disk full"));

    let after = worker_hosts::heartbeat(
        &pool, host.id, true, false, None, None, None, 0, None, None, None, None, None,
    )
    .await
    .expect("omitted last_error");
    assert_eq!(
        after.last_error, None,
        "heartbeat(..., last_error=None) must clear the previous error"
    );
}

/// platform/hostname use COALESCE($4/$5); omitted None must keep the last strings.
#[tokio::test]
async fn heartbeat_omitted_platform_and_hostname_keep_previous() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "hb-ident", expires).await;
    pair_host(&pool, &token, "box-hb-ident").await;

    let planted = worker_hosts::heartbeat(
        &pool,
        host.id,
        true,
        false,
        Some("darwin-arm64"),
        Some("box-hb-ident-2"),
        None,
        0,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("plant platform/hostname");
    assert_eq!(planted.platform.as_deref(), Some("darwin-arm64"));
    assert_eq!(planted.hostname.as_deref(), Some("box-hb-ident-2"));

    let after = worker_hosts::heartbeat(
        &pool, host.id, true, false, None, None, None, 0, None, None, None, None, None,
    )
    .await
    .expect("omitted platform/hostname");
    assert_eq!(
        after.platform.as_deref(),
        Some("darwin-arm64"),
        "omitted platform must COALESCE and keep the last string"
    );
    assert_eq!(
        after.hostname.as_deref(),
        Some("box-hb-ident-2"),
        "omitted hostname must COALESCE and keep the last string"
    );
}

/// Missing row is NotFound (fetch_optional none). Siblings only cover disabled WHERE.
#[tokio::test]
async fn heartbeat_unknown_id_is_not_found() {
    let pool = connect_and_migrate().await;
    let missing = Uuid::now_v7();

    let err = worker_hosts::heartbeat(
        &pool, missing, true, false, None, None, None, 0, None, None, None, None, None,
    )
    .await
    .expect_err("unknown host id");
    match err {
        DbError::NotFound(msg) => assert!(
            msg.contains("not found"),
            "missing host must be NotFound: {msg}"
        ),
        other => panic!("expected NotFound on missing host heartbeat, got {other:?}"),
    }
    assert!(
        worker_hosts::get(&pool, missing)
            .await
            .expect("get")
            .is_none(),
        "unknown heartbeat must not insert a host"
    );
}

/// Some(machine_id)+Some(runtimes) persist as JSONB; $14/$15 must not swap.
#[tokio::test]
async fn heartbeat_with_machine_persists_agent_runtimes() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("runtimes");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    let mid = Uuid::now_v7();
    worker_hosts::consume_pair_token(
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
}

/// Later Some(json) replaces the stored inventory; it does not merge leftover agents.
#[tokio::test]
async fn heartbeat_with_machine_replaces_agent_runtimes() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("rt-replace");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let first = json!([
        {
            "id": "grok",
            "name": "Grok",
            "installed": true,
            "logged_in": true,
            "models": ["grok-4"],
            "model_details": []
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
    heartbeat_with_runtimes(&pool, host.id, None, Some(first)).await;

    let replaced = json!([{
        "id": "ollama",
        "name": "Ollama",
        "installed": true,
        "logged_in": false,
        "models": ["llama3"],
        "model_details": []
    }]);
    let after = heartbeat_with_runtimes(&pool, host.id, None, Some(replaced.clone())).await;
    assert_eq!(
        after.agent_runtimes, replaced,
        "a later Some(json) must replace, not merge"
    );
}

/// Heartbeat must not adopt another host's machine_id; runtimes still persist.
#[tokio::test]
async fn heartbeat_with_machine_does_not_adopt_another_hosts_machine_id() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);

    let name_a = unique_slug("rt-mid-a");
    let (host_a, token_a) =
        create_pending_with_token(&pool, workspace_id, user_id, &name_a, expires).await;
    let mid_a = Uuid::now_v7();
    worker_hosts::consume_pair_token(
        &pool,
        &token_a,
        &format!("{name_a}-box"),
        "linux-x86_64",
        true,
        false,
        Some(mid_a),
    )
    .await
    .expect("pair A");

    let name_b = unique_slug("rt-mid-b");
    let (_host_b, token_b) =
        create_pending_with_token(&pool, workspace_id, user_id, &name_b, expires).await;
    let mid_b = Uuid::now_v7();
    worker_hosts::consume_pair_token(
        &pool,
        &token_b,
        &format!("{name_b}-box"),
        "linux-x86_64",
        true,
        false,
        Some(mid_b),
    )
    .await
    .expect("pair B");

    let runtimes = json!([{
        "id": "grok",
        "name": "Grok",
        "installed": true,
        "logged_in": true,
        "models": ["grok-4"],
        "model_details": []
    }]);
    let after =
        heartbeat_with_runtimes(&pool, host_a.id, Some(mid_b), Some(runtimes.clone())).await;
    assert_eq!(
        after.machine_id,
        Some(mid_a),
        "heartbeat must not adopt another host's machine_id"
    );
    assert_eq!(
        after.agent_runtimes, runtimes,
        "refusing a taken machine_id must still persist agent_runtimes"
    );
}

/// Omitting agent_runtimes (old fleet-host / heartbeat() wrapper) must
/// COALESCE-keep the last CLI inventory. get + as_public.agents must agree
/// with UPDATE RETURNING so a RETURNING-only lie cannot stay green.
#[tokio::test]
async fn heartbeat_with_machine_none_does_not_wipe_agent_runtimes() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("keep-rt");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let runtimes = json!([{
        "id": "grok",
        "name": "Grok",
        "installed": true,
        "logged_in": true,
        "models": ["grok-4"],
        "model_details": []
    }]);
    heartbeat_with_runtimes(&pool, host.id, None, Some(runtimes.clone())).await;

    let omitted = heartbeat_with_runtimes(&pool, host.id, None, None).await;
    assert_eq!(
        omitted.agent_runtimes, runtimes,
        "None must COALESCE and leave the last report (not reset to [])"
    );

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(
        loaded.agent_runtimes, runtimes,
        "omit must persist the last inventory, not only RETURNING it"
    );
    assert_eq!(
        loaded
            .as_public(Utc::now())
            .agents
            .iter()
            .map(|a| a.id.as_str())
            .collect::<Vec<_>>(),
        vec!["grok"],
        "as_public.agents must still list grok after omit"
    );

    let via_wrapper = worker_hosts::heartbeat(
        &pool, host.id, true, false, None, None, None, 0, None, None, None, None, None,
    )
    .await
    .expect("legacy heartbeat wrapper");
    assert_eq!(
        via_wrapper.agent_runtimes, runtimes,
        "heartbeat() always passes None and must not wipe agent_runtimes"
    );
}

/// Some([]) is a real empty report and must persist, unlike omitted None.
#[tokio::test]
async fn heartbeat_with_machine_empty_agent_runtimes_persists() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("empty-rt");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    heartbeat_with_runtimes(
        &pool,
        host.id,
        None,
        Some(json!([{
            "id": "grok",
            "name": "Grok",
            "installed": true,
            "logged_in": true,
            "models": ["grok-4"],
            "model_details": []
        }])),
    )
    .await;

    let cleared = heartbeat_with_runtimes(&pool, host.id, None, Some(json!([]))).await;
    assert_eq!(
        cleared.agent_runtimes,
        json!([]),
        "Some([]) is a real empty report and must persist, unlike omitted None"
    );

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(loaded.agent_runtimes, json!([]));
    assert!(
        loaded.as_public(Utc::now()).agents.is_empty(),
        "as_public.agents must be empty after an empty report"
    );
}

/// Some(hardware) persists the CPU/RAM/GPU snapshot as JSONB.
#[tokio::test]
async fn heartbeat_persists_hardware_inventory() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("hw-persist");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let inventory = json!({
        "cpu_model": "Intel(R) Core(TM) i7-10610U CPU @ 1.80GHz",
        "cpu_cores": 8,
        "mem_total_bytes": 17179869184u64,
        "mem_type": "DDR4",
        "gpus": [{"name": "Intel UHD Graphics", "used_pct": 4}],
        "os": "linux 6.8 x86_64"
    });
    let stored = heartbeat_with_hardware(&pool, host.id, Some(inventory.clone())).await;
    assert_eq!(
        stored.hardware,
        Some(inventory),
        "Some(hardware) must persist the CPU/RAM/GPU snapshot as-is"
    );
}

/// sanitize_heartbeat_hardware drops empty / trademark-only GPU names before persist.
#[tokio::test]
async fn heartbeat_persists_hardware_inventory_empty_gpu_name_dropped() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("hw-inv");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let mixed = json!({
        "cpu_model": "Intel(R) Core(TM) i7-10610U CPU @ 1.80GHz",
        "cpu_cores": 8,
        "gpus": [
            {"name": ""},
            {"name": "   "},
            {"name": "(R)"},
            {"name": null},
            {},
            {"name": "Intel UHD Graphics", "used_pct": 4}
        ]
    });
    let stored = heartbeat_with_hardware(&pool, host.id, Some(mixed)).await;
    assert_eq!(
        stored.hardware.as_ref().and_then(|h| h.get("gpus")),
        Some(&json!([{"name": "Intel UHD Graphics", "used_pct": 4}])),
        "trademark-only (R) and nameless GPUs must be gone; named iGPU stays"
    );
}

/// Later Some(json) replaces the hardware snapshot; leftover keys do not merge.
#[tokio::test]
async fn heartbeat_replaces_hardware_inventory() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("hw-replace");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    heartbeat_with_hardware(
        &pool,
        host.id,
        Some(json!({
            "cpu_cores": 8,
            "mem_type": "DDR4",
            "gpus": [{"name": "Intel UHD Graphics"}]
        })),
    )
    .await;
    let replaced = json!({
        "cpu_cores": 4,
        "mem_total_bytes": 8589934592u64,
        "gpus": [{"name": "Intel UHD Graphics"}]
    });
    let after = heartbeat_with_hardware(&pool, host.id, Some(replaced.clone())).await;
    assert_eq!(
        after.hardware,
        Some(replaced),
        "later Some(json) must replace, not merge leftover keys"
    );
}

/// Omitting hardware must COALESCE-keep the last snapshot.
#[tokio::test]
async fn heartbeat_omitted_hardware_does_not_wipe_inventory() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("hw-omit");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let inventory = json!({
        "cpu_cores": 8,
        "mem_type": "DDR4",
        "gpus": [{"name": "Intel UHD Graphics", "used_pct": 4}]
    });
    heartbeat_with_hardware(&pool, host.id, Some(inventory.clone())).await;
    let omitted = heartbeat_with_hardware(&pool, host.id, None).await;
    assert_eq!(
        omitted.hardware,
        Some(inventory),
        "None must COALESCE and leave the last CPU/RAM/GPU inventory (not wipe to null)"
    );
}

/// Some({}) is a real empty snapshot and must persist, unlike omitted None.
#[tokio::test]
async fn heartbeat_empty_hardware_snapshot_persists() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("hw-empty");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    heartbeat_with_hardware(
        &pool,
        host.id,
        Some(json!({
            "cpu_cores": 8,
            "gpus": [{"name": "Intel UHD Graphics"}]
        })),
    )
    .await;
    let cleared = heartbeat_with_hardware(&pool, host.id, Some(json!({}))).await;
    assert_eq!(
        cleared.hardware,
        Some(json!({})),
        "Some({{}}) is a real empty report and must persist, unlike omitted None"
    );
}

/// Sanitize retains the raw GPU name; as_public normalizes trademarks.
#[tokio::test]
async fn heartbeat_stores_raw_gpu_name_public_card_normalizes() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("hw-raw");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let raw = json!({"gpus": [{"name": "Intel(R) UHD Graphics"}]});
    let stored = heartbeat_with_hardware(&pool, host.id, Some(raw.clone())).await;
    assert_eq!(
        stored.hardware,
        Some(raw),
        "sanitize retains the reported name; it does not rewrite trademarks"
    );
    let pub_name = stored
        .as_public(Utc::now())
        .hardware
        .as_ref()
        .and_then(|h| h.gpus.as_ref())
        .and_then(|g| g.first())
        .and_then(|g| g.name.as_deref())
        .map(str::to_owned);
    assert_eq!(
        pub_name.as_deref(),
        Some("Intel UHD Graphics"),
        "as_public must strip (R) on the card"
    );
}

/// Nameless GPUs persist as []; unknown/Xeon CPU must not invent Intel UHD.
#[tokio::test]
async fn heartbeat_nameless_gpus_unknown_cpu_public_has_no_gpu() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("hw-xeon");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let report = json!({
        "cpu_model": "Intel(R) Xeon(R) Gold 6248R CPU @ 3.00GHz",
        "gpus": [{"name": ""}, {"name": "  "}, {"name": null}, {}]
    });
    let stored = heartbeat_with_hardware(&pool, host.id, Some(report)).await;
    assert_eq!(
        stored.hardware.as_ref().and_then(|h| h.get("gpus")),
        Some(&json!([])),
        "nameless GPU entries must persist as an empty array"
    );
    let pub_gpus = stored
        .as_public(Utc::now())
        .hardware
        .as_ref()
        .and_then(|h| h.gpus.clone())
        .unwrap_or_default();
    assert!(
        pub_gpus.is_empty(),
        "Xeon must not infer a phantom Intel UHD: {pub_gpus:?}"
    );
}

/// PATCH `enabled=false` is a distinct picker skip from revoke (status/hash)
/// or paused/drain. Unpaired pending is status/hash/last_seen, not enabled.
#[tokio::test]
async fn list_assignable_excludes_paused_disabled_stale() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);

    let (ok_host, ok_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "ok", expires).await;
    pair_host(&pool, &ok_token, "ok-box").await;

    let (off_host, off_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "off", expires).await;
    pair_host(&pool, &off_token, "off-box").await;
    let off = worker_hosts::patch_cp_settings(
        &pool,
        off_host.id,
        &HostCpSettings {
            enabled: Some(false),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("disable enabled flag");
    assert!(!off.enabled);
    assert_eq!(off.status, "online");

    let (_pending, _) =
        create_pending_with_token(&pool, workspace_id, user_id, "still-pending", expires).await;

    let assignable = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
        .await
        .expect("list_assignable");
    let ids = assignable_ids_in_workspace(&assignable, workspace_id);
    assert_eq!(
        ids,
        vec![ok_host.id],
        "enabled=false and unpaired pending must not take a lease: {ids:?}"
    );
}

/// Cutoff is the passed `stale_after`, not a hardcoded 90s.
#[tokio::test]
async fn list_assignable_uses_passed_stale_after_not_90s_constant() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "cutoff", expires).await;
    pair_host(&pool, &token, "cutoff-box").await;
    sqlx::query(
        "UPDATE worker_hosts SET last_seen_at = now() - interval '45 seconds' WHERE id = $1",
    )
    .bind(host.id)
    .execute(&pool)
    .await
    .expect("age last_seen_at 45s");

    let tight = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
        .await
        .expect("list_assignable 30s");
    assert!(
        !assignable_ids_in_workspace(&tight, workspace_id).contains(&host.id),
        "45s-old last_seen must miss a 30s cutoff: {:?}",
        tight.iter().map(|h| h.id).collect::<Vec<_>>()
    );

    let loose = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(90))
        .await
        .expect("list_assignable 90s");
    assert!(
        assignable_ids_in_workspace(&loose, workspace_id).contains(&host.id),
        "45s-old last_seen must hit a 90s cutoff: {:?}",
        loose.iter().map(|h| h.id).collect::<Vec<_>>()
    );
}

/// Cluster OR sits inside the hold filters; a paused Kind box must not lease in B.
#[tokio::test]
async fn list_assignable_excludes_paused_cluster_host_from_other_workspace() {
    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (_user_b, ws_b) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, ws_a, user_a, "cluster-paused", expires).await;
    pair_host(&pool, &token, "cluster-paused").await;
    sqlx::query("UPDATE worker_hosts SET labels = array['cluster'] WHERE id = $1")
        .bind(host.id)
        .execute(&pool)
        .await
        .expect("label cluster");
    let paused = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            paused: Some(true),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("pause cluster host");
    assert!(paused.paused);
    assert!(paused.is_cluster_host());

    let from_b = worker_hosts::list_assignable(&pool, ws_b, Duration::seconds(30))
        .await
        .expect("list_assignable B");
    assert!(
        from_b.iter().all(|h| h.id != host.id),
        "paused cluster host home={ws_a} must not be assignable in {ws_b}: {:?}",
        from_b.iter().map(|h| h.id).collect::<Vec<_>>()
    );
}

/// PATCH `enabled=true` returns the host; hold skip is paused/drain/modes, not enabled.
#[tokio::test]
async fn list_assignable_reincludes_after_enabled_true() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "flip-enabled", expires).await;
    pair_host(&pool, &token, "flip-enabled-box").await;

    let off = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            enabled: Some(false),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("enabled=false");
    assert!(!off.enabled);
    let omitted = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
        .await
        .expect("list_assignable while disabled");
    assert!(
        !assignable_ids_in_workspace(&omitted, workspace_id).contains(&host.id),
        "enabled=false must drop the host: {:?}",
        omitted.iter().map(|h| h.id).collect::<Vec<_>>()
    );

    let on = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            enabled: Some(true),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("enabled=true");
    assert!(on.enabled);
    let assignable = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
        .await
        .expect("list_assignable after re-enable");
    assert_eq!(
        assignable_ids_in_workspace(&assignable, workspace_id),
        vec![host.id],
        "enabled=true must return the host to the picker: {:?}",
        assignable.iter().map(|h| h.id).collect::<Vec<_>>()
    );
}

/// Paused/drain/no-modes with a running job must leave the picker.
/// A running job may finish; they must not receive another lease.
#[tokio::test]
async fn list_assignable_skips_holding_new_jobs_even_with_running_job() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);

    async fn pair_with_running_job(
        pool: &fleet_db::PgPool,
        workspace_id: Uuid,
        user_id: Uuid,
        expires: chrono::DateTime<Utc>,
        name: &str,
    ) -> Uuid {
        let (host, token) =
            create_pending_with_token(pool, workspace_id, user_id, name, expires).await;
        pair_host(pool, &token, &format!("{name}-box")).await;
        worker_hosts::heartbeat(
            pool, host.id, true, false, None, None, None, 1, None, None, None, None, None,
        )
        .await
        .expect("heartbeat current_jobs=1");
        host.id
    }

    let taking = pair_with_running_job(&pool, workspace_id, user_id, expires, "taking").await;
    let paused = pair_with_running_job(&pool, workspace_id, user_id, expires, "paused-hold").await;
    let drain = pair_with_running_job(&pool, workspace_id, user_id, expires, "drain-hold").await;
    let no_modes =
        pair_with_running_job(&pool, workspace_id, user_id, expires, "no-modes-hold").await;

    worker_hosts::patch_cp_settings(
        &pool,
        paused,
        &HostCpSettings {
            paused: Some(true),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("paused");
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

    let assignable = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
        .await
        .expect("list_assignable");
    assert_eq!(
        assignable_ids_in_workspace(&assignable, workspace_id),
        vec![taking],
        "paused/drain/no-modes with current_jobs=1 must skip the picker: {:?}",
        assignable.iter().map(|h| h.id).collect::<Vec<_>>()
    );
}

/// accepting_jobs is OR: one plane mode on stays in the picker (both-off is the hold).
#[tokio::test]
async fn list_assignable_includes_when_only_one_plane_mode_on() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);

    let (host_only, host_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "host-only", expires).await;
    pair_host(&pool, &host_token, "host-only-box").await;
    worker_hosts::heartbeat(
        &pool, host_only.id, true, false, None, None, None, 1, None, None, None, None, None,
    )
    .await
    .expect("heartbeat host-only current_jobs=1");
    worker_hosts::patch_cp_settings(
        &pool,
        host_only.id,
        &HostCpSettings {
            allow_host: Some(true),
            allow_container: Some(false),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("host-only");

    let (container_only, container_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "container-only", expires).await;
    pair_host(&pool, &container_token, "container-only-box").await;
    worker_hosts::heartbeat(
        &pool,
        container_only.id,
        true,
        false,
        None,
        None,
        None,
        1,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("heartbeat container-only current_jobs=1");
    worker_hosts::patch_cp_settings(
        &pool,
        container_only.id,
        &HostCpSettings {
            allow_host: Some(false),
            allow_container: Some(true),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("container-only");

    let assignable = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
        .await
        .expect("list_assignable");
    let mut ids = assignable_ids_in_workspace(&assignable, workspace_id);
    ids.sort();
    let mut want = vec![host_only.id, container_only.id];
    want.sort();
    assert_eq!(
        ids, want,
        "host-only and container-only with current_jobs=1 must stay assignable"
    );
}

/// PATCH drain=false after drain + a running job returns the host to the picker.
#[tokio::test]
async fn list_assignable_take_jobs_again_after_drain() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "drain-resume", expires).await;
    pair_host(&pool, &token, "drain-resume-box").await;
    worker_hosts::heartbeat(
        &pool, host.id, true, false, None, None, None, 1, None, None, None, None, None,
    )
    .await
    .expect("heartbeat current_jobs=1");
    worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            drain: Some(true),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("drain");

    let resumed = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            drain: Some(false),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("take jobs again");
    assert!(!resumed.drain);

    let assignable = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
        .await
        .expect("list_assignable after drain=false");
    assert_eq!(
        assignable_ids_in_workspace(&assignable, workspace_id),
        vec![host.id],
        "drain=false must return the host to the picker: {:?}",
        assignable.iter().map(|h| h.id).collect::<Vec<_>>()
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
        from_b.iter().all(|h| h.id != host.id),
        "CLUSTER host home={ws_a} must not be assignable in foreign {ws_b}: {:?}",
        from_b.iter().map(|h| h.id).collect::<Vec<_>>()
    );
    let from_a = worker_hosts::list_assignable(&pool, ws_a, Duration::seconds(30))
        .await
        .expect("list_assignable A");
    assert!(
        from_a.iter().any(|h| h.id == host.id),
        "home workspace still lists its bootstrap host"
    );
}

#[tokio::test]
async fn create_pending_defaults_send_policy_owner_only() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, _token) =
        create_pending_with_token(&pool, workspace_id, user_id, "maya-pc", expires).await;
    assert_eq!(host.send_policy, fleet_db::WORKER_HOST_SEND_OWNER_ONLY);
    assert_eq!(host.created_by, Some(user_id));
}

#[tokio::test]
async fn remove_member_drops_send_allowlist_rows() {
    let pool = connect_and_migrate().await;
    let (maya, ws) = seed_user_with_workspace(&pool).await;
    let jordan = seed_user(&pool, &format!("jordan-{}@ex.test", Uuid::now_v7())).await;
    workspaces::add_member(&pool, ws, jordan, "member", Utc::now())
        .await
        .expect("add jordan");
    let expires = Utc::now() + Duration::hours(1);
    let (host, _token) = create_pending_with_token(&pool, ws, maya, "maya-pc", expires).await;
    worker_hosts::set_send_policy(&pool, host.id, fleet_db::WORKER_HOST_SEND_ALLOWLIST)
        .await
        .expect("allowlist");
    worker_hosts::replace_send_allowlist(&pool, host.id, &[jordan], maya)
        .await
        .expect("allow jordan");
    let before = worker_hosts::list_send_allowlist(&pool, host.id)
        .await
        .expect("list");
    assert_eq!(before, vec![jordan]);
    workspaces::remove_member(&pool, ws, jordan)
        .await
        .expect("remove");
    let after = worker_hosts::list_send_allowlist(&pool, host.id)
        .await
        .expect("list after");
    assert!(after.is_empty(), "remove_member must drop allowlist rows");
}

/// create_pending INSERT stores mixed-case `CLUSTER` as lowercase `cluster`.
#[tokio::test]
async fn create_pending_normalizes_cluster_label() {
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
        "create_pending must persist lowercase cluster"
    );
}

/// patch_cp_settings UPDATE stores mixed-case `Cluster` as lowercase `cluster`.
#[tokio::test]
async fn patch_cp_settings_normalizes_cluster_label() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, _token) = create_pending_with_token(
        &pool,
        workspace_id,
        user_id,
        &format!("patch-cluster-{}", Uuid::now_v7()),
        expires,
    )
    .await;
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
        "patch_cp_settings must persist lowercase cluster"
    );
}

/// `labels: None` leaves the column (Kind unscope survives a name/paused patch).
#[tokio::test]
async fn patch_cp_settings_omitted_labels_keep_cluster() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let mut new = NewWorkerHost::new(
        Uuid::now_v7(),
        workspace_id,
        format!("keep-cluster-{}", Uuid::now_v7()),
        Some(user_id),
    );
    new.labels = vec!["cluster".into()];
    let created = worker_hosts::create_pending(
        &pool,
        &new,
        &hash_token(&generate_pair_token()),
        expires,
    )
    .await
    .expect("create_pending");
    assert_eq!(created.labels, vec!["cluster".to_string()]);

    let patched = worker_hosts::patch_cp_settings(
        &pool,
        created.id,
        &HostCpSettings {
            name: Some("renamed-keep-cluster".into()),
            paused: Some(true),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("patch name/paused with labels omitted");
    assert_eq!(patched.name, "renamed-keep-cluster");
    assert!(patched.paused);
    assert_eq!(
        patched.labels,
        vec!["cluster".to_string()],
        "omitted labels must leave the cluster token"
    );
}

/// create_pending must not lowercase non-cluster tokens.
#[tokio::test]
async fn create_pending_preserves_non_cluster_label_case() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let mut new = NewWorkerHost::new(
        Uuid::now_v7(),
        workspace_id,
        format!("case-labels-{}", Uuid::now_v7()),
        Some(user_id),
    );
    new.labels = vec!["GPU".into(), "Linux".into()];
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
        vec!["GPU".to_string(), "Linux".to_string()],
        "create_pending must persist non-cluster label case"
    );
}

/// Some([]) replaces labels; the host leaves list_cluster.
#[tokio::test]
async fn patch_cp_settings_empty_labels_clears_cluster() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let mut new = NewWorkerHost::new(
        Uuid::now_v7(),
        workspace_id,
        format!("clear-cluster-{}", Uuid::now_v7()),
        Some(user_id),
    );
    new.labels = vec!["cluster".into()];
    let created = worker_hosts::create_pending(
        &pool,
        &new,
        &hash_token(&generate_pair_token()),
        expires,
    )
    .await
    .expect("create_pending");
    assert_eq!(created.labels, vec!["cluster".to_string()]);

    let cleared = worker_hosts::patch_cp_settings(
        &pool,
        created.id,
        &HostCpSettings {
            labels: Some(vec![]),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("patch empty labels");
    assert_eq!(
        cleared.labels,
        Vec::<String>::new(),
        "Some([]) must replace labels, not keep cluster"
    );

    let clustered = worker_hosts::list_cluster(&pool)
        .await
        .expect("list_cluster");
    assert!(
        clustered.iter().all(|h| h.id != created.id),
        "empty labels must leave list_cluster: {:?}",
        clustered.iter().map(|h| h.id).collect::<Vec<_>>()
    );
}

/// Replacing leftover [cluster, gpu] with [gpu] drops the host from a
/// foreign workspace picker; home workspace stays.
#[tokio::test]
async fn clearing_inferred_cluster_label_removes_cross_workspace_assignability() {
    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (_user_b, ws_b) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = format!("clear-cluster-{}", Uuid::now_v7());
    let mut new = NewWorkerHost::new(Uuid::now_v7(), ws_a, name.clone(), Some(user_a));
    new.labels = vec!["cluster".into(), "gpu".into()];
    let token = generate_pair_token();
    let host = worker_hosts::create_pending(&pool, &new, &hash_token(&token), expires)
        .await
        .expect("create_pending");
    pair_host(&pool, &token, &name).await;

    let cleared = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            labels: Some(vec!["gpu".into()]),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("clear cluster");
    assert_eq!(
        cleared.labels,
        vec!["gpu".to_string()],
        "patch must drop cluster and keep other labels"
    );
    assert_eq!(cleared.workspace_id, ws_a);

    let from_b = worker_hosts::list_assignable(&pool, ws_b, Duration::seconds(30))
        .await
        .expect("list_assignable B");
    assert!(
        from_b.iter().all(|h| h.id != host.id),
        "cleared cluster host home={ws_a} must not be assignable in {ws_b}: {:?}",
        from_b.iter().map(|h| h.id).collect::<Vec<_>>()
    );

    let from_a = worker_hosts::list_assignable(&pool, ws_a, Duration::seconds(30))
        .await
        .expect("list_assignable A");
    assert!(
        from_a.iter().any(|h| h.id == host.id),
        "home workspace must still assign the host after clearing cluster: {:?}",
        from_a.iter().map(|h| h.id).collect::<Vec<_>>()
    );

    let clustered = worker_hosts::list_cluster(&pool)
        .await
        .expect("list_cluster");
    assert!(
        clustered.iter().all(|h| h.id != host.id),
        "cleared host must not remain a cluster box: {:?}",
        clustered.iter().map(|h| h.id).collect::<Vec<_>>()
    );
}

/// `labels: None` clones current; a paused=false patch must leave cluster
/// and keep the host assignable in a foreign workspace.
#[tokio::test]
async fn patch_cp_settings_omitted_labels_keeps_cluster_cross_ws() {
    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (_user_b, ws_b) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = format!("omit-cluster-{}", Uuid::now_v7());
    let mut new = NewWorkerHost::new(Uuid::now_v7(), ws_a, name.clone(), Some(user_a));
    new.labels = vec!["cluster".into()];
    let token = generate_pair_token();
    let host = worker_hosts::create_pending(&pool, &new, &hash_token(&token), expires)
        .await
        .expect("create_pending");
    pair_host(&pool, &token, &name).await;

    let patched = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            labels: None,
            paused: Some(false),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("patch paused with labels omitted");
    assert_eq!(
        patched.labels,
        vec!["cluster".to_string()],
        "omitted labels must leave the cluster token"
    );
    assert!(!patched.paused);

    let from_b = worker_hosts::list_assignable(&pool, ws_b, Duration::seconds(30))
        .await
        .expect("list_assignable B");
    assert!(
        from_b.iter().any(|h| h.id == host.id),
        "cluster host home={ws_a} must stay assignable in {ws_b} when labels are omitted: {:?}",
        from_b.iter().map(|h| h.id).collect::<Vec<_>>()
    );
}

/// Some([]) persists [] and drops the host from list_cluster and a foreign
/// workspace picker; home workspace stays.
#[tokio::test]
async fn patch_cp_settings_empty_labels_unscopes() {
    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (_user_b, ws_b) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = format!("empty-unscope-{}", Uuid::now_v7());
    let mut new = NewWorkerHost::new(Uuid::now_v7(), ws_a, name.clone(), Some(user_a));
    new.labels = vec!["cluster".into()];
    let token = generate_pair_token();
    let host = worker_hosts::create_pending(&pool, &new, &hash_token(&token), expires)
        .await
        .expect("create_pending");
    pair_host(&pool, &token, &name).await;

    let cleared = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            labels: Some(vec![]),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("patch empty labels");
    assert_eq!(
        cleared.labels,
        Vec::<String>::new(),
        "Some([]) must persist empty labels"
    );

    let clustered = worker_hosts::list_cluster(&pool)
        .await
        .expect("list_cluster");
    assert!(
        clustered.iter().all(|h| h.id != host.id),
        "empty labels must leave list_cluster: {:?}",
        clustered.iter().map(|h| h.id).collect::<Vec<_>>()
    );

    let from_b = worker_hosts::list_assignable(&pool, ws_b, Duration::seconds(30))
        .await
        .expect("list_assignable B");
    assert!(
        from_b.iter().all(|h| h.id != host.id),
        "empty-label host home={ws_a} must not be assignable in {ws_b}: {:?}",
        from_b.iter().map(|h| h.id).collect::<Vec<_>>()
    );

    let from_a = worker_hosts::list_assignable(&pool, ws_a, Duration::seconds(30))
        .await
        .expect("list_assignable A");
    assert!(
        from_a.iter().any(|h| h.id == host.id),
        "home workspace must still assign the host after empty labels: {:?}",
        from_a.iter().map(|h| h.id).collect::<Vec<_>>()
    );
}

/// 023 strips inferred `cluster` from a UI-paired container-only row
/// (`created_by` set) and keeps it on bootstrap (`created_by IS NULL`).
#[tokio::test]
async fn cluster_label_only_from_bootstrap_null_created_by_migration_023() {
    const UNDO: &str = include_str!("../migrations/023_undo_inferred_cluster_label.sql");

    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);

    let ui_name = format!("ui-inferred-{}", Uuid::now_v7());
    let mut ui_new = NewWorkerHost::new(Uuid::now_v7(), ws_a, ui_name.clone(), Some(user_a));
    ui_new.labels = vec!["cluster".into(), "gpu".into()];
    let ui_token = generate_pair_token();
    worker_hosts::create_pending(&pool, &ui_new, &hash_token(&ui_token), expires)
        .await
        .expect("create_pending UI");
    let ui = worker_hosts::consume_pair_token(
        &pool,
        &ui_token,
        &ui_name,
        "linux-x86_64",
        false,
        true,
        None,
    )
    .await
    .expect("pair container-only UI box");
    assert_eq!(ui.host.created_by, Some(user_a));
    assert!(!ui.host.advertised_host && ui.host.advertised_container);
    assert_eq!(
        ui.host.labels,
        vec!["cluster".to_string(), "gpu".to_string()]
    );

    let boot =
        create_bootstrap_cluster_host(&pool, ws_a, "kind-bootstrap", expires, false, true).await;
    assert!(boot.host.created_by.is_none());
    assert_eq!(boot.host.labels, vec!["cluster".to_string()]);

    apply_label_migration_to_hosts(&pool, UNDO, &[ui.host.id, boot.host.id]).await;

    let ui_023 = worker_hosts::get(&pool, ui.host.id)
        .await
        .expect("get UI after 023")
        .expect("UI exists");
    assert_eq!(
        ui_023.labels,
        vec!["gpu".to_string()],
        "023 must drop inferred cluster and keep other labels"
    );
    assert_eq!(ui_023.created_by, Some(user_a));

    let boot_023 = worker_hosts::get(&pool, boot.host.id)
        .await
        .expect("get bootstrap after 023")
        .expect("bootstrap exists");
    assert!(
        boot_023.created_by.is_none(),
        "023 must not invent created_by on bootstrap"
    );
    assert_eq!(
        boot_023.labels,
        vec!["cluster".to_string()],
        "023 must keep cluster on created_by IS NULL bootstrap: {:?}",
        boot_023.labels
    );
}

/// 023's advertised_container AND NOT advertised_host predicate: leftover
/// [cluster, gpu] on a UI-paired host-advertising box stays.
#[tokio::test]
async fn migration_023_keeps_cluster_on_host_advertising_created_by_set() {
    const UNDO: &str = include_str!("../migrations/023_undo_inferred_cluster_label.sql");

    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = format!("host-adv-cluster-{}", Uuid::now_v7());
    let mut new = NewWorkerHost::new(Uuid::now_v7(), workspace_id, name.clone(), Some(user_id));
    new.labels = vec!["cluster".into(), "gpu".into()];
    let token = generate_pair_token();
    worker_hosts::create_pending(&pool, &new, &hash_token(&token), expires)
        .await
        .expect("create_pending");
    let paired = pair_host(&pool, &token, &name).await;
    assert_eq!(paired.host.created_by, Some(user_id));
    assert!(paired.host.advertised_host && !paired.host.advertised_container);
    assert_eq!(
        paired.host.labels,
        vec!["cluster".to_string(), "gpu".to_string()]
    );

    apply_label_migration_to_hosts(&pool, UNDO, &[paired.host.id]).await;

    let after = worker_hosts::get(&pool, paired.host.id)
        .await
        .expect("get after 023")
        .expect("host exists");
    assert_eq!(
        after.labels,
        vec!["cluster".to_string(), "gpu".to_string()],
        "023 must not strip cluster from a host-advertising UI-paired box: {:?}",
        after.labels
    );
}

/// The CP laptop is shared with every workspace on this plane without a
/// `cluster` label (023 strips cluster from UI-paired hosts). Extra
/// hostnames must list + assign; a foreign hostname must stay hidden.
#[tokio::test]
async fn extra_hostnames_make_this_machine_visible_and_assignable_across_workspaces() {
    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (_user_b, ws_b) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let hostname = format!("dev-laptop-{}", Uuid::new_v4());
    let (host, token) =
        create_pending_with_token(&pool, ws_a, user_a, "this-machine", expires).await;
    pair_host(&pool, &token, &hostname).await;

    let from_b = worker_hosts::list_assignable(&pool, ws_b, Duration::seconds(30))
        .await
        .expect("list_assignable B");
    assert!(
        from_b.iter().all(|h| h.id != host.id),
        "non-cluster host must not be assignable in a foreign workspace without extra hostnames: {:?}",
        from_b.iter().map(|h| h.id).collect::<Vec<_>>()
    );

    let visible_empty = worker_hosts::list_visible_for_workspace(&pool, ws_b, &[])
        .await
        .expect("visible empty");
    assert!(
        visible_empty.iter().all(|h| h.id != host.id),
        "empty extra hostnames must not leak a foreign host"
    );

    let extra = vec![hostname.clone()];
    let visible = worker_hosts::list_visible_for_workspace(&pool, ws_b, &extra)
        .await
        .expect("visible extra");
    assert!(
        visible.iter().any(|h| h.id == host.id),
        "this-machine hostname must list in a foreign workspace: {:?}",
        visible.iter().map(|h| h.id).collect::<Vec<_>>()
    );

    let assignable = worker_hosts::list_assignable_with_extra_hostnames(
        &pool,
        ws_b,
        Duration::seconds(30),
        &extra,
    )
    .await
    .expect("assignable extra");
    assert!(
        assignable.iter().any(|h| h.id == host.id),
        "this-machine hostname must be assignable in a foreign workspace: {:?}",
        assignable.iter().map(|h| h.id).collect::<Vec<_>>()
    );

    let fqdn = format!("{hostname}.lan");
    let via_short = worker_hosts::list_assignable_with_extra_hostnames(
        &pool,
        ws_b,
        Duration::seconds(30),
        &[fqdn],
    )
    .await
    .expect("assignable fqdn needle");
    assert!(
        via_short.iter().any(|h| h.id == host.id),
        "FQDN extra needle must match short hostname: {:?}",
        via_short.iter().map(|h| h.id).collect::<Vec<_>>()
    );

    let other = worker_hosts::list_assignable_with_extra_hostnames(
        &pool,
        ws_b,
        Duration::seconds(30),
        &[format!("other-box-{}", Uuid::new_v4())],
    )
    .await
    .expect("assignable other");
    assert!(
        other.iter().all(|h| h.id != host.id),
        "a different extra hostname must not pull this host"
    );
}

/// consume_pair_token must not infer `cluster` on a container-only UI pair.
#[tokio::test]
async fn consume_pair_token_does_not_stamp_cluster_on_container_only() {
    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (_user_b, ws_b) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (pending, token) =
        create_pending_with_token(&pool, ws_a, user_a, "ui-container", expires).await;
    assert_eq!(pending.labels, Vec::<String>::new());
    assert_eq!(pending.created_by, Some(user_a));

    let paired = worker_hosts::consume_pair_token(
        &pool,
        &token,
        "ui-container",
        "linux-x86_64",
        false,
        true,
        None,
    )
    .await
    .expect("pair container-only");
    assert_eq!(
        paired.host.labels,
        Vec::<String>::new(),
        "consume_pair_token must not stamp cluster: {:?}",
        paired.host.labels
    );
    assert!(!paired.host.advertised_host && paired.host.advertised_container);

    let from_b = worker_hosts::list_assignable(&pool, ws_b, Duration::seconds(30))
        .await
        .expect("list_assignable B");
    assert!(
        from_b.iter().all(|h| h.id != paired.host.id),
        "container-only pair home={ws_a} must not be assignable in {ws_b}: {:?}",
        from_b.iter().map(|h| h.id).collect::<Vec<_>>()
    );
}

/// Paired sandboxed placeholder drops from the picker; a native sibling stays.
#[tokio::test]
async fn list_assignable_excludes_sandboxed_isolation() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);

    let (native_host, native_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "native", expires).await;
    pair_host(&pool, &native_token, "native-box").await;

    let (sandboxed_host, sandboxed_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "sandboxed", expires).await;
    pair_host(&pool, &sandboxed_token, "sandboxed-box").await;
    worker_hosts::patch_cp_settings(
        &pool,
        sandboxed_host.id,
        &HostCpSettings {
            isolation: Some("sandboxed".into()),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("mark sandboxed");

    let assignable = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
        .await
        .expect("list_assignable");
    let ids = assignable_ids_in_workspace(&assignable, workspace_id);
    assert_eq!(
        ids,
        vec![native_host.id],
        "sandboxed host is a placeholder and must not be assignable: {ids:?}"
    );
}

/// Isolation AND applies after workspace OR CLUSTER_LABEL_SQL.
#[tokio::test]
async fn list_assignable_excludes_sandboxed_cluster_host_from_other_workspace() {
    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (_user_b, ws_b) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, ws_a, user_a, "cluster-sandboxed", expires).await;
    pair_host(&pool, &token, "cluster-sandboxed").await;
    sqlx::query("UPDATE worker_hosts SET labels = array['cluster'] WHERE id = $1")
        .bind(host.id)
        .execute(&pool)
        .await
        .expect("label cluster");
    worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            isolation: Some("sandboxed".into()),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("mark sandboxed");

    let from_b = worker_hosts::list_assignable(&pool, ws_b, Duration::seconds(30))
        .await
        .expect("list_assignable B");
    assert!(
        from_b.iter().all(|h| h.id != host.id),
        "sandboxed cluster host home={ws_a} must not be assignable in {ws_b}: {:?}",
        from_b.iter().map(|h| h.id).collect::<Vec<_>>()
    );
}

/// Picker skip is not a delete; operators can PATCH the sandboxed row back.
#[tokio::test]
async fn list_for_workspace_still_includes_sandboxed_placeholder() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "sandboxed-keep", expires).await;
    pair_host(&pool, &token, "sandboxed-keep").await;
    worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            isolation: Some("sandboxed".into()),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("mark sandboxed");

    let listed = worker_hosts::list_for_workspace(&pool, workspace_id)
        .await
        .expect("list_for_workspace");
    let found = listed
        .iter()
        .find(|h| h.id == host.id)
        .expect("sandboxed placeholder must stay in list_for_workspace");
    assert_eq!(found.isolation, "sandboxed");

    let got = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(got.isolation, "sandboxed");
}

/// Unknown isolation is Conflict; the column stays native and assignable.
#[tokio::test]
async fn patch_cp_settings_unknown_isolation_is_conflict() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "k8s-reject", expires).await;
    pair_host(&pool, &token, "k8s-reject").await;

    let err = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            isolation: Some("k8s".into()),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect_err("unknown isolation");
    match err {
        DbError::Conflict(msg) => assert_eq!(
            msg, "isolation must be native or sandboxed",
            "repo parse reject: {msg}"
        ),
        other => panic!("expected Conflict, got {other:?}"),
    }

    let stored = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(stored.isolation, "native");

    let assignable = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
        .await
        .expect("list_assignable");
    assert!(
        assignable_ids_in_workspace(&assignable, workspace_id).contains(&host.id),
        "rejected isolation must leave the host assignable: {:?}",
        assignable.iter().map(|h| h.id).collect::<Vec<_>>()
    );
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
/// `payload->>'host_id'` matches. Home enqueue makes an always-None filter fail.
#[tokio::test]
async fn claim_next_spawn_for_host_in_workspace_does_not_lease_foreign_workspace() {
    let pool = connect_and_migrate().await;
    let ws_a = seed_user_with_workspace(&pool).await.1;
    let ws_b = seed_user_with_workspace(&pool).await.1;
    let host_a = Uuid::now_v7();

    let home = spawn_claim(ws_a, host_a, "spawn_session");
    let foreign = spawn_claim(ws_b, host_a, "spawn_session");
    work_claims::enqueue(&pool, &home)
        .await
        .expect("enqueue home spawn");
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
    .expect("scoped claim")
    .expect("home-workspace spawn must be leased");
    assert_eq!(scoped.id, home.id);
    assert_eq!(scoped.workspace_id, ws_a);
    assert_eq!(scoped.lease_owner, Some(host_a));

    let leftover = work_claims::get(&pool, foreign.id)
        .await
        .expect("get foreign")
        .expect("foreign row remains");
    assert_eq!(leftover.status, "ready");
    assert_eq!(leftover.workspace_id, ws_b);
    assert!(leftover.lease_owner.is_none());
}

/// Sibling 212 only filters host_id on the unscoped path.
#[tokio::test]
async fn claim_next_spawn_for_host_in_workspace_does_not_lease_other_host_same_workspace() {
    let pool = connect_and_migrate().await;
    let ws_a = seed_user_with_workspace(&pool).await.1;
    let host_a = Uuid::now_v7();
    let host_b = Uuid::now_v7();

    let other_host = spawn_claim(ws_a, host_b, "spawn_session");
    work_claims::enqueue(&pool, &other_host)
        .await
        .expect("enqueue host B spawn");

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
        "host A must not take host B spawn in the same workspace: {scoped:?}"
    );

    let leftover = work_claims::get(&pool, other_host.id)
        .await
        .expect("get host B")
        .expect("host B row remains");
    assert_eq!(leftover.status, "ready");
    assert!(leftover.lease_owner.is_none());
}

/// Kind-gated $5 would stay green if only spawn_session were enqueued.
#[tokio::test]
async fn claim_next_spawn_for_host_in_workspace_filters_signal_session_and_chat_turn() {
    let pool = connect_and_migrate().await;
    let ws_a = seed_user_with_workspace(&pool).await.1;
    let ws_b = seed_user_with_workspace(&pool).await.1;
    let host_a = Uuid::now_v7();

    let signal = spawn_claim(ws_b, host_a, "signal_session");
    let chat = spawn_claim(ws_b, host_a, "chat_turn");
    work_claims::enqueue(&pool, &signal)
        .await
        .expect("enqueue foreign signal");
    work_claims::enqueue(&pool, &chat)
        .await
        .expect("enqueue foreign chat");

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
        "home-workspace claim must not take foreign signal_session or chat_turn: {scoped:?}"
    );

    let leftover_signal = work_claims::get(&pool, signal.id)
        .await
        .expect("get signal")
        .expect("signal row remains");
    let leftover_chat = work_claims::get(&pool, chat.id)
        .await
        .expect("get chat")
        .expect("chat row remains");
    assert_eq!(leftover_signal.status, "ready");
    assert_eq!(leftover_chat.status, "ready");
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

    let leftover = work_claims::get(&pool, leased.id)
        .await
        .expect("get")
        .expect("row remains");
    assert_eq!(leftover.status, "leased");
    assert_eq!(leftover.lease_owner, Some(host_a));
    assert_eq!(leftover.lease_token, Some(token));
}

/// Expired leased row returns to ready and drops the token; a still-valid
/// sibling lease is untouched. `reclaim_expired_leases` is the SQL the
/// HTTP lease poll calls first.
#[tokio::test]
async fn reclaim_expired_leases_repo_ready_clears_token_leaves_unexpired() {
    let pool = connect_and_migrate().await;
    let (_user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let host_id = Uuid::now_v7();

    let aged_new = spawn_claim(workspace_id, host_id, "spawn_session");
    let live_new = spawn_claim(workspace_id, host_id, "spawn_session");
    work_claims::enqueue(&pool, &aged_new)
        .await
        .expect("enqueue aged");
    work_claims::enqueue(&pool, &live_new)
        .await
        .expect("enqueue live");

    let first =
        work_claims::claim_next_spawn_for_host(&pool, workspace_id, host_id, Duration::minutes(2))
            .await
            .expect("claim first")
            .expect("first leased");
    let second =
        work_claims::claim_next_spawn_for_host(&pool, workspace_id, host_id, Duration::minutes(2))
            .await
            .expect("claim second")
            .expect("second leased");
    let claimed = [first, second];
    assert!(
        claimed.iter().any(|c| c.id == aged_new.id),
        "aged row leased"
    );
    let live_leased = claimed
        .iter()
        .find(|c| c.id == live_new.id)
        .expect("live row leased");
    let live_token = live_leased.lease_token.expect("live lease_token");

    let aged = sqlx::query(
        r#"
        UPDATE work_claims
        SET lease_expires_at = now() - interval '1 minute'
        WHERE id = $1
        "#,
    )
    .bind(aged_new.id)
    .execute(&pool)
    .await
    .expect("backdate aged lease");
    assert_eq!(
        aged.rows_affected(),
        1,
        "expected the leased row aged: {}",
        aged_new.id
    );

    let n = work_claims::reclaim_expired_leases(&pool, 64)
        .await
        .expect("reclaim_expired_leases");
    assert!(n >= 1, "must reclaim the aged leased row: {n}");

    let aged_row = work_claims::get(&pool, aged_new.id)
        .await
        .expect("get aged")
        .expect("aged row remains");
    assert_eq!(
        aged_row.status, "ready",
        "aged must return to ready: {aged_row:?}"
    );
    assert!(
        aged_row.lease_token.is_none(),
        "aged lease_token cleared: {aged_row:?}"
    );
    assert!(
        aged_row.lease_owner.is_none(),
        "aged lease_owner cleared: {aged_row:?}"
    );
    assert!(
        aged_row.lease_expires_at.is_none(),
        "aged lease_expires_at cleared: {aged_row:?}"
    );

    let live_row = work_claims::get(&pool, live_new.id)
        .await
        .expect("get live")
        .expect("live row remains");
    assert_eq!(
        live_row.status, "leased",
        "unexpired sibling stays leased: {live_row:?}"
    );
    assert_eq!(
        live_row.lease_token,
        Some(live_token),
        "unexpired sibling keeps original token: {live_row:?}"
    );
    assert_eq!(live_row.lease_owner, Some(host_id));
}

/// `WHERE status = 'leased'` must not re-queue finished work. Plant done
/// and dead rows with `lease_expires_at` in the past; reclaim leaves
/// status and version unchanged.
#[tokio::test]
async fn reclaim_expired_leases_does_not_revive_done_or_dead() {
    let pool = connect_and_migrate().await;
    let (_user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let host_id = Uuid::now_v7();

    let done_new = spawn_claim(workspace_id, host_id, "spawn_session");
    let mut dead_new = spawn_claim(workspace_id, host_id, "spawn_session");
    dead_new.max_attempts = 1;
    work_claims::enqueue(&pool, &done_new)
        .await
        .expect("enqueue done");
    work_claims::enqueue(&pool, &dead_new)
        .await
        .expect("enqueue dead");

    let first =
        work_claims::claim_next_spawn_for_host(&pool, workspace_id, host_id, Duration::minutes(2))
            .await
            .expect("claim first")
            .expect("first leased");
    let second =
        work_claims::claim_next_spawn_for_host(&pool, workspace_id, host_id, Duration::minutes(2))
            .await
            .expect("claim second")
            .expect("second leased");
    let claimed = [first, second];
    let done_leased = claimed
        .iter()
        .find(|c| c.id == done_new.id)
        .expect("done row leased");
    let dead_leased = claimed
        .iter()
        .find(|c| c.id == dead_new.id)
        .expect("dead row leased");
    let done_token = done_leased.lease_token.expect("done lease_token");
    let dead_token = dead_leased.lease_token.expect("dead lease_token");

    let finished = work_claims::complete(&pool, done_leased.id, done_token)
        .await
        .expect("complete done");
    assert!(finished, "done row must complete");
    let killed = work_claims::fail(&pool, dead_leased.id, dead_token, "plant dead", None)
        .await
        .expect("fail dead");
    assert!(killed, "dead row must fail");

    let planted = sqlx::query(
        r#"
        UPDATE work_claims
        SET lease_expires_at = now() - interval '1 minute'
        WHERE id IN ($1, $2)
        "#,
    )
    .bind(done_new.id)
    .bind(dead_new.id)
    .execute(&pool)
    .await
    .expect("backdate done/dead expiry");
    assert_eq!(planted.rows_affected(), 2, "expected both finished rows aged");

    let before_done = work_claims::get(&pool, done_new.id)
        .await
        .expect("get done before")
        .expect("done row");
    let before_dead = work_claims::get(&pool, dead_new.id)
        .await
        .expect("get dead before")
        .expect("dead row");
    assert_eq!(before_done.status, "done", "{before_done:?}");
    assert_eq!(before_dead.status, "dead", "{before_dead:?}");

    let _ = work_claims::reclaim_expired_leases(&pool, 64)
        .await
        .expect("reclaim_expired_leases");

    let after_done = work_claims::get(&pool, done_new.id)
        .await
        .expect("get done after")
        .expect("done row remains");
    let after_dead = work_claims::get(&pool, dead_new.id)
        .await
        .expect("get dead after")
        .expect("dead row remains");
    assert_eq!(
        after_done.status, "done",
        "done must not return to ready: {after_done:?}"
    );
    assert_eq!(
        after_dead.status, "dead",
        "dead must not return to ready: {after_dead:?}"
    );
    assert_eq!(
        after_done.version, before_done.version,
        "done version must stay put: {after_done:?}"
    );
    assert_eq!(
        after_dead.version, before_dead.version,
        "dead version must stay put: {after_dead:?}"
    );
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

    assert_pair_token_plaintext_column_absent_or_null(&pool, host.id).await;
    assert_pair_token_hashed_at_rest(&pool, host.id, &token).await;
    assert_repo_host_omits_pair_plaintext(&host, &[&token]);
    assert_repo_reads_omit_pair_plaintext(&pool, workspace_id, host.id, &[&token]).await;
}

/// replace_pair_token SET used_at on unused rows; consume of the old flp_
/// is Conflict already used (a DELETE of the old row must not stay green).
#[tokio::test]
async fn replace_pair_token_only_while_pending_and_invalidates_old() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, old_token) =
        create_pending_with_token(&pool, workspace_id, user_id, "rotate", expires).await;

    worker_hosts::replace_pair_token(
        &pool,
        host.id,
        &hash_token(&generate_pair_token()),
        expires,
    )
    .await
    .expect("replace while pending");

    let (_, used_at) = pair_token_hash_and_used_at(&pool, host.id, &old_token).await;
    assert!(
        used_at.is_some(),
        "replace must SET used_at on the unused create-time token"
    );

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
    assert_pair_conflict(err, "already used");
}

/// Age last_seen past 90s → as_public offline; heartbeat (not re-pair) advances
/// last_seen and remaps public online. Picker stale-drop is a sibling.
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

    let aged = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    let aged_seen = aged.last_seen_at.expect("aged last_seen_at");
    assert_eq!(
        aged.as_public(Utc::now()).status,
        "offline",
        "API/UI must report offline after 90s without a heartbeat"
    );

    let revived = worker_hosts::heartbeat(
        &pool, host.id, true, false, None, None, None, 0, None, None, None, None, None,
    )
    .await
    .expect("heartbeat");
    let seen = revived.last_seen_at.expect("heartbeat sets last_seen_at");
    assert!(
        seen > aged_seen,
        "heartbeat must advance last_seen past the aged stamp"
    );
    assert_eq!(
        revived.as_public(Utc::now()).status,
        "online",
        "heartbeat without re-pair must report public online"
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
    let queued = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            install_model: Some(spec.clone()),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("queue install_request");
    let requested_at = queued
        .install_requested_at
        .expect("install_requested_at must be set");

    // Ok(unchanged) and Err are equally correct; product is keep-queued.
    let _ = worker_hosts::clear_install_request_if(&pool, host.id, Some("other-spec")).await;

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(
        loaded.install_request.as_ref(),
        Some(&spec),
        "queued install_request must survive a mismatched ack"
    );
    assert_eq!(
        loaded.install_requested_at,
        Some(requested_at),
        "install_requested_at must stay the pre-act stamp after a mismatch"
    );
}

/// Omitted/empty/whitespace ack must not drop a queued install (old hosts omit spec).
#[tokio::test]
async fn clear_install_request_if_none_empty_or_whitespace_does_not_clear() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("inst-blank-ack");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let spec = json!({ "provider": "ollama", "spec": "llama3" });
    let queued = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            install_model: Some(spec.clone()),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("queue install_request");
    let requested_at = queued
        .install_requested_at
        .expect("install_requested_at must be set");

    for ack in [None, Some(""), Some("   ")] {
        let _ = worker_hosts::clear_install_request_if(&pool, host.id, ack).await;
        let loaded = worker_hosts::get(&pool, host.id)
            .await
            .expect("get")
            .expect("host");
        assert_eq!(
            loaded.install_request.as_ref(),
            Some(&spec),
            "queued install_request must survive omitted/empty/whitespace ack {ack:?}"
        );
        assert_eq!(
            loaded.install_requested_at,
            Some(requested_at),
            "install_requested_at must stay the pre-act stamp after omitted/empty/whitespace ack {ack:?}"
        );
    }
}

/// `str::trim` runs before the SQL match; padded llama3 must clear both columns.
#[tokio::test]
async fn clear_install_request_if_trims_ack_spec_before_match() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("inst-trim");
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

    let cleared = worker_hosts::clear_install_request_if(&pool, host.id, Some("  llama3  "))
        .await
        .expect("padded matching spec must clear");
    assert!(
        cleared.install_request.is_none(),
        "trimmed ack must drop install_request"
    );
    assert!(
        cleared.install_requested_at.is_none(),
        "trimmed ack must drop install_requested_at"
    );

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert!(loaded.install_request.is_none());
    assert!(loaded.install_requested_at.is_none());
}

/// Queued JSON without a spec key (`->>'spec'` is NULL) must not clear on llama3 ack.
#[tokio::test]
async fn clear_install_request_if_queued_json_without_spec_key_does_not_clear() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("inst-nospec");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let spec = json!({ "provider": "ollama" });
    let queued = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            install_model: Some(spec.clone()),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("queue install_request without spec");
    assert!(
        queued
            .install_request
            .as_ref()
            .and_then(|v| v.get("spec"))
            .is_none(),
        "fixture must omit the spec key"
    );
    let requested_at = queued
        .install_requested_at
        .expect("install_requested_at must be set");

    let _ = worker_hosts::clear_install_request_if(&pool, host.id, Some("llama3")).await;

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(
        loaded.install_request.as_ref(),
        Some(&spec),
        "install_request without spec key must survive a llama3 ack"
    );
    assert_eq!(
        loaded.install_requested_at,
        Some(requested_at),
        "install_requested_at must stay the pre-act stamp when ->>'spec' is NULL"
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

/// Seed two keys; null one and add another so the unmentioned key survives.
#[tokio::test]
async fn patch_runtime_settings_merges_and_null_drops_key() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("rt-settings");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let first = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            runtime_settings: Some(json!({ "harness": "grok", "work_dir": "/tmp/work" })),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("seed two keys");

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
        json!({ "work_dir": "/tmp/work", "effort": "high" }),
        "unmentioned work_dir must survive; harness dropped; effort added"
    );
    assert!(
        second.settings_gen > first.settings_gen,
        "runtime_settings change must bump settings_gen ({gen} vs {first})",
        gen = second.settings_gen,
        first = first.settings_gen
    );

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(
        loaded.runtime_settings,
        json!({ "work_dir": "/tmp/work", "effort": "high" })
    );
    assert_eq!(loaded.settings_gen, second.settings_gen);
}

/// `runtime_settings: None` clones the stored object; only the other field bumps gen.
#[tokio::test]
async fn patch_runtime_settings_omitted_none_leaves_stored() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("rt-omit");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let seeded = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            runtime_settings: Some(json!({ "work_dir": "/tmp/work" })),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("seed work_dir");

    let patched = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            allow_host: Some(false),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("allow_host only");
    assert_eq!(
        patched.runtime_settings,
        json!({ "work_dir": "/tmp/work" }),
        "omitted runtime_settings must leave the stored object"
    );
    assert!(!patched.allow_host);
    assert_eq!(
        patched.settings_gen,
        seeded.settings_gen + 1,
        "only the allow_host change must bump settings_gen"
    );

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(loaded.runtime_settings, json!({ "work_dir": "/tmp/work" }));
    assert_eq!(loaded.settings_gen, patched.settings_gen);
}

/// Re-sending the already-stored object must not bump settings_gen.
#[tokio::test]
async fn patch_runtime_settings_identical_object_does_not_bump_gen() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("rt-same");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    let first = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            runtime_settings: Some(json!({ "effort": "high" })),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("seed effort");

    let again = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            runtime_settings: Some(json!({ "effort": "high" })),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("identical object");
    assert_eq!(again.runtime_settings, json!({ "effort": "high" }));
    assert_eq!(
        again.settings_gen, first.settings_gen,
        "identical runtime_settings must not bump settings_gen"
    );

    let loaded = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host");
    assert_eq!(loaded.runtime_settings, json!({ "effort": "high" }));
    assert_eq!(loaded.settings_gen, first.settings_gen);
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

/// PATCH 0 / -5 / 512 / 9999 must clamp then persist 1 or 256 (not reject, not write raw).
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

    let zero = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            max_concurrent: Some(0),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("PATCH 0 must clamp");
    assert_eq!(zero.max_concurrent, 1);
    let stored = worker_hosts::get(&pool, host.id)
        .await
        .expect("get after 0")
        .expect("host");
    assert_eq!(stored.max_concurrent, 1, "stored after PATCH 0");

    let neg = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            max_concurrent: Some(-5),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("PATCH -5 must clamp");
    assert_eq!(neg.max_concurrent, 1);
    let stored = worker_hosts::get(&pool, host.id)
        .await
        .expect("get after -5")
        .expect("host");
    assert_eq!(stored.max_concurrent, 1, "stored after PATCH -5");

    let mid = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            max_concurrent: Some(512),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("PATCH 512 must clamp");
    assert_eq!(mid.max_concurrent, 256);
    let stored = worker_hosts::get(&pool, host.id)
        .await
        .expect("get after 512")
        .expect("host");
    assert_eq!(stored.max_concurrent, 256, "stored after PATCH 512");

    let huge = worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            max_concurrent: Some(9999),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("PATCH 9999 must clamp");
    assert_eq!(huge.max_concurrent, 256);
    let stored = worker_hosts::get(&pool, host.id)
        .await
        .expect("get after 9999")
        .expect("host");
    assert_eq!(stored.max_concurrent, 256, "stored after PATCH 9999");
}

/// Advertised 0 persists Some(1) without writing the plane column; omitted
/// advertised must COALESCE-keep that 1 so effective stays 1.
#[tokio::test]
async fn heartbeat_omitted_advertised_after_zero_keeps_clamped_one() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let name = unique_slug("maxc-eff");
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, &name, expires).await;
    pair_host(&pool, &token, &format!("{name}-box")).await;

    worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            max_concurrent: Some(8),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("set plane 8");

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
        zero_adv.max_concurrent, 8,
        "heartbeat must not copy advertised into the plane column"
    );

    let omitted = worker_hosts::heartbeat(
        &pool,
        host.id,
        true,
        false,
        None,
        None,
        None,
        1,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .expect("omit advertised");
    assert_eq!(
        omitted.advertised_max_concurrent,
        Some(1),
        "omitted advertised must COALESCE-keep the clamped 1"
    );
    assert_eq!(omitted.max_concurrent, 8);
    assert_eq!(
        worker_hosts::effective_max_concurrent(&omitted),
        1,
        "effective must stay 1 after omitted advertised"
    );
}

/// Hosts at min(plane, advertised) must drop out of list_assignable; under-cap peers stay.
/// Advertised-min catches SQL `current_jobs < max_concurrent`; plane_wins catches
/// COALESCE(advertised, plane) instead of min().
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
    let plane_wins = pair_named(&pool, workspace_id, user_id, expires, "plane-wins").await;

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

    let assignable = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
        .await
        .expect("list_assignable");
    let ids = assignable_ids_in_workspace(&assignable, workspace_id);
    assert_eq!(
        ids,
        vec![under],
        "advertised-min and plane-wins at cap must skip; under-cap stays: {ids:?}"
    );
}

/// Heartbeat 2 of min(8,2) then 1 of 2 must return the host to the picker.
#[tokio::test]
async fn list_assignable_returns_host_when_current_jobs_drops_below_cap() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "below-cap-again", expires).await;
    pair_host(&pool, &token, "below-cap-again-box").await;
    worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            max_concurrent: Some(8),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("plane 8");
    worker_hosts::heartbeat(
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
    .expect("2 of min(8,2)");
    worker_hosts::heartbeat(
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
    .expect("1 of min(8,2)");

    let assignable = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
        .await
        .expect("list_assignable");
    assert_eq!(
        assignable_ids_in_workspace(&assignable, workspace_id),
        vec![host.id],
        "1 of min(8,2) after 2 of 2 must return to the picker"
    );
}

/// current_jobs above the effective cap must skip (`>=`, not only equality).
#[tokio::test]
async fn list_assignable_skips_when_current_jobs_exceeds_effective_cap() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "over-cap", expires).await;
    pair_host(&pool, &token, "over-cap-box").await;
    worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            max_concurrent: Some(8),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("plane 8");
    worker_hosts::heartbeat(
        &pool,
        host.id,
        true,
        false,
        None,
        None,
        None,
        3,
        Some(2),
        None,
        None,
        None,
        None,
    )
    .await
    .expect("3 of min(8,2)");

    let assignable = worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
        .await
        .expect("list_assignable");
    let ids = assignable_ids_in_workspace(&assignable, workspace_id);
    assert!(
        !ids.contains(&host.id),
        "current_jobs=3 of min(8,2) must skip: {ids:?}"
    );
}

/// Cluster OR sits inside the rust at_job_cap filter; a full Kind box must not lease in B.
#[tokio::test]
async fn list_assignable_skips_at_cap_cluster_host_from_other_workspace() {
    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (_user_b, ws_b) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, ws_a, user_a, "cluster-at-cap", expires).await;
    pair_host(&pool, &token, "cluster-at-cap").await;
    sqlx::query("UPDATE worker_hosts SET labels = array['cluster'] WHERE id = $1")
        .bind(host.id)
        .execute(&pool)
        .await
        .expect("label cluster");
    worker_hosts::patch_cp_settings(
        &pool,
        host.id,
        &HostCpSettings {
            max_concurrent: Some(8),
            ..HostCpSettings::default()
        },
    )
    .await
    .expect("plane 8");
    worker_hosts::heartbeat(
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
    .expect("2 of min(8,2)");

    let from_b = worker_hosts::list_assignable(&pool, ws_b, Duration::seconds(30))
        .await
        .expect("list_assignable B");
    assert!(
        from_b.iter().all(|h| h.id != host.id),
        "at-cap cluster host home={ws_a} must not be assignable in {ws_b}: {:?}",
        from_b.iter().map(|h| h.id).collect::<Vec<_>>()
    );
}
