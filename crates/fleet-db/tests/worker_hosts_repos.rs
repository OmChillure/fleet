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
    worker_hosts::consume_pair_token(pool, token, hostname, "linux-x86_64", true, false, None)
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

    let err =
        worker_hosts::consume_pair_token(&pool, &token, "box-1", "linux-x86_64", true, false, None)
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
        "box",
        "linux-x86_64",
        true,
        false,
        Some(mid),
    )
    .await
    .expect("first pair");
    assert_eq!(paired.host.id, first.id);
    assert_eq!(paired.host.machine_id, Some(mid));

    worker_hosts::revoke(&pool, first.id).await.expect("remove");

    let (second, token_b) =
        create_pending_with_token(&pool, workspace_id, user_id, "box-again", expires).await;
    let again = worker_hosts::consume_pair_token(
        &pool,
        &token_b,
        "box",
        "linux-x86_64",
        true,
        false,
        Some(mid),
    )
    .await
    .expect("re-pair");
    assert_eq!(again.host.id, first.id, "must revive the original row");
    assert_eq!(again.host.status, "online");
    assert!(again.host.enabled);
    assert_eq!(again.host.machine_id, Some(mid));

    let leftover = worker_hosts::get(&pool, second.id)
        .await
        .expect("get leftover")
        .expect("pending leftover exists");
    assert_eq!(leftover.status, "disabled");
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

    let err =
        worker_hosts::consume_pair_token(&pool, &token, "box", "linux-x86_64", true, false, None)
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
    let ids: Vec<Uuid> = assignable.iter().map(|h| h.id).collect();
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
    let both_ids: Vec<Uuid> = both.iter().map(|h| h.id).collect();
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
        worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
            .await
            .expect("assignable")
            .is_empty(),
        "stale host must not be scheduled"
    );
}
