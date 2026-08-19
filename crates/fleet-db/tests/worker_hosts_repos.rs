//! Fail-closed Postgres tests for paired worker hosts.
//! Requires DATABASE_URL — never skips.

mod common;

use chrono::{Duration, Utc};
use fleet_db::error::DbError;
use fleet_db::models::{HostCpSettings, NewWorkClaim, NewWorkerHost};
use fleet_db::repos::users::NewUser;
use fleet_db::repos::worker_hosts::{self, PAIR_TOKEN_PREFIX, generate_pair_token, hash_token};
use fleet_db::repos::{users, work_claims, workspaces};
use serde_json::json;
use uuid::Uuid;

use common::{connect_and_migrate, unique_email, unique_slug};

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
    worker_hosts::consume_pair_token(pool, token, hostname, "linux-x86_64", true, false)
        .await
        .expect("consume_pair_token")
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
    assert!(
        paired
            .machine_token
            .starts_with(worker_hosts::MACHINE_TOKEN_PREFIX)
    );

    let authed = worker_hosts::authenticate_machine(&pool, &paired.machine_token)
        .await
        .expect("auth")
        .expect("machine token must authenticate");
    assert_eq!(authed.id, host.id);

    let err = worker_hosts::consume_pair_token(&pool, &token, "box-1", "linux-x86_64", true, false)
        .await
        .expect_err("second consume must fail");
    match err {
        DbError::Conflict(msg) => assert!(
            msg.contains("used") || msg.contains("pending"),
            "unexpected conflict: {msg}"
        ),
        other => panic!("expected Conflict on second consume, got {other:?}"),
    }
}

#[tokio::test]
async fn expired_pair_token_fails() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expired_at = Utc::now() - Duration::seconds(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "stale-token", expired_at).await;

    let err = worker_hosts::consume_pair_token(&pool, &token, "box", "linux-x86_64", true, false)
        .await
        .expect_err("expired pair token must fail");
    match err {
        DbError::Conflict(msg) => {
            assert!(msg.contains("expired"), "unexpected conflict: {msg}");
        }
        other => panic!("expected Conflict for expired token, got {other:?}"),
    }

    let still = worker_hosts::get(&pool, host.id)
        .await
        .expect("get")
        .expect("host remains");
    assert_eq!(still.status, "pending");
    assert!(still.machine_token_hash.is_none());
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

    let load =
        worker_hosts::heartbeat(&pool, host.id, false, true, None, None, None, -3, Some(512))
            .await
            .expect("heartbeat load");
    assert_eq!(load.current_jobs, 0, "current_jobs must clamp >= 0");
    assert_eq!(
        load.advertised_max_concurrent,
        Some(256),
        "advertised_max_concurrent must clamp to 256"
    );

    let mid = worker_hosts::heartbeat(&pool, host.id, false, true, None, None, None, 2, Some(8))
        .await
        .expect("heartbeat mid");
    assert_eq!(mid.current_jobs, 2);
    assert_eq!(mid.advertised_max_concurrent, Some(8));

    let keep = worker_hosts::heartbeat(&pool, host.id, false, true, None, None, None, 1, None)
        .await
        .expect("heartbeat keep advertised");
    assert_eq!(keep.current_jobs, 1);
    assert_eq!(
        keep.advertised_max_concurrent,
        Some(8),
        "None must leave advertised_max_concurrent unchanged"
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
    let ids: Vec<Uuid> = assignable.iter().map(|h| h.id).collect();
    assert_eq!(
        ids,
        vec![ok_host.id],
        "only the fresh online host is assignable"
    );
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
async fn pair_token_is_stored_hashed_not_plaintext() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);
    let (host, token) =
        create_pending_with_token(&pool, workspace_id, user_id, "hashed", expires).await;

    let stored: String = sqlx::query_scalar(
        "SELECT token_hash FROM worker_host_pair_tokens WHERE host_id = $1 AND used_at IS NULL",
    )
    .bind(host.id)
    .fetch_one(&pool)
    .await
    .expect("pair token row");
    assert_eq!(stored, hash_token(&token));
    assert_ne!(stored, token);
    assert!(
        !stored.contains(&token),
        "plaintext pair token must not appear in the hash column"
    );
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

    let err =
        worker_hosts::consume_pair_token(&pool, &old_token, "box", "linux-x86_64", true, false)
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

    sqlx::query("UPDATE worker_hosts SET last_seen_at = now() - interval '2 minutes' WHERE id = $1")
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
        "API/UI must report offline after 30s without a heartbeat"
    );
    assert!(
        worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
            .await
            .expect("assignable")
            .is_empty(),
        "stale host must not be scheduled"
    );
}
