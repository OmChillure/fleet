//! Fail-closed Postgres repository tests for fleet-db.
//! Requires DATABASE_URL — never skips.
//!
//! Criticizes expected behavior at the repo boundary (not HTTP).

mod common;

use chrono::{Duration, SubsecRound, Utc};
use fleet_db::error::DbError;
use fleet_db::models::{NewAuthSession, NewWorkerHost, PartnersDisclaimerAck};
use fleet_db::repos::users::NewUser;
use fleet_db::repos::{cases, chats, sessions, tickets, users, worker_hosts, workspaces};
use serde_json::json;
use uuid::Uuid;

use common::{connect_and_migrate, unique_email, unique_slug};

/// Seed an active user inside a committed transaction; return its id.
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

// 1. users::insert_tx — duplicate email_normalized

#[tokio::test]
async fn insert_tx_duplicate_email_normalized_is_conflict_with_exact_message() {
    let pool = connect_and_migrate().await;
    let email = unique_email();
    let normalized = email.to_lowercase();
    let now = Utc::now();

    let mut tx = pool.begin().await.expect("begin first");
    users::insert_tx(
        &mut tx,
        &NewUser {
            id: Uuid::now_v7(),
            email: email.clone(),
            email_normalized: normalized.clone(),
            display_name: Some("First".into()),
            password_hash: "hash-a".into(),
            created_at: now,
        },
    )
    .await
    .expect("first insert");
    tx.commit().await.expect("commit first");

    // Same normalized email, different casing / display — must still conflict.
    let mut tx = pool.begin().await.expect("begin second");
    let err = users::insert_tx(
        &mut tx,
        &NewUser {
            id: Uuid::now_v7(),
            email: email.to_uppercase(),
            email_normalized: normalized,
            display_name: Some("Second".into()),
            password_hash: "hash-b".into(),
            created_at: now,
        },
    )
    .await
    .expect_err("duplicate email_normalized must fail");
    let _ = tx.rollback().await;

    match &err {
        DbError::Conflict(msg) => {
            assert_eq!(
                msg, "an account with this email already exists",
                "insert_tx must check 23505 BEFORE from_sqlx so the domain \
                 message is preserved (not the generic unique-constraint text)"
            );
        }
        other => panic!("expected DbError::Conflict, got {other:?}"),
    }
    // Still a unique violation for generic callers.
    assert!(
        err.is_unique_violation(),
        "Conflict from insert_tx must count as unique violation"
    );
}

// 2. workspace slug unique → Conflict

#[tokio::test]
async fn workspace_slug_unique_is_conflict() {
    let pool = connect_and_migrate().await;
    let email = unique_email();
    let user_id = seed_user(&pool, &email).await;
    let slug = unique_slug("shared");
    let now = Utc::now();

    workspaces::create_owned(&pool, Uuid::now_v7(), user_id, &slug, "One", now)
        .await
        .expect("first workspace");

    // Second owner on same slug — unique index workspaces_slug_uidx.
    let email2 = unique_email();
    let user2 = seed_user(&pool, &email2).await;
    let err = workspaces::create_owned(&pool, Uuid::now_v7(), user2, &slug, "Two", now)
        .await
        .expect_err("duplicate slug must fail");

    match &err {
        DbError::Conflict(msg) => {
            assert_eq!(
                msg, "unique constraint violated",
                "map_query_err/from_sqlx maps 23505 to this generic Conflict message"
            );
        }
        other => panic!("expected DbError::Conflict from unique slug, got {other:?}"),
    }
    assert!(
        err.is_unique_violation(),
        "mapped Conflict from 23505 must be is_unique_violation (workspaces route relies on this)"
    );
}

// 3. session insert + find by refresh hash

#[tokio::test]
async fn session_insert_and_find_by_refresh_hash() {
    let pool = connect_and_migrate().await;
    let email = unique_email();
    let user_id = seed_user(&pool, &email).await;

    let session_id = Uuid::now_v7();
    let family_id = Uuid::now_v7();
    // Unique 32-byte hash per run (simulates SHA-256 of refresh token).
    let mut hash = [0u8; 32];
    hash[..16].copy_from_slice(session_id.as_bytes());
    hash[16..].copy_from_slice(family_id.as_bytes());

    let now = Utc::now();
    let new = NewAuthSession {
        id: session_id,
        user_id,
        refresh_token_hash: hash.to_vec(),
        family_id,
        user_agent: Some("fleet-db-test".into()),
        ip: None,
        expires_at: now + Duration::hours(1),
        absolute_expires_at: now + Duration::days(7),
    };

    sessions::insert(&pool, &new).await.expect("insert session");

    let found = sessions::find_by_refresh_hash(&pool, &hash)
        .await
        .expect("find by hash")
        .expect("session row must exist");

    assert_eq!(found.id, session_id);
    assert_eq!(found.user_id, user_id);
    assert_eq!(found.family_id, family_id);
    assert_eq!(found.refresh_token_hash, hash.to_vec());
    assert!(found.revoked_at.is_none());
    assert!(found.is_active(now));

    // Wrong hash → None (not an error).
    let missing = sessions::find_by_refresh_hash(&pool, &[0u8; 32])
        .await
        .expect("find missing");
    assert!(missing.is_none(), "unknown refresh hash must yield None");
}

// 4. chat conversation belongs to user+workspace

#[tokio::test]
async fn chat_conversation_scoped_to_user_and_workspace() {
    let pool = connect_and_migrate().await;
    let (owner_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let (other_id, _other_ws) = seed_user_with_workspace(&pool).await;

    let conv_id = Uuid::now_v7();
    let now = Utc::now();
    let created = chats::create_conversation(
        &pool,
        conv_id,
        workspace_id,
        owner_id,
        "scoped chat",
        Some("ollama"),
        Some("test-model"),
        now,
    )
    .await
    .expect("create conversation");
    assert_eq!(created.id, conv_id);
    assert_eq!(created.user_id, owner_id);
    assert_eq!(created.workspace_id, workspace_id);

    let ok = chats::get_conversation(&pool, conv_id, workspace_id, owner_id)
        .await
        .expect("get as owner")
        .expect("owner must see their conversation");
    assert_eq!(ok.id, conv_id);

    // Other user id in the same workspace → None (not another user's chat).
    let denied = chats::get_conversation(&pool, conv_id, workspace_id, other_id)
        .await
        .expect("get as other");
    assert!(
        denied.is_none(),
        "conversation must not be readable by a different user_id"
    );

    // Owner with wrong workspace → None.
    let wrong_ws = chats::get_conversation(&pool, conv_id, Uuid::now_v7(), owner_id)
        .await
        .expect("get wrong workspace");
    assert!(
        wrong_ws.is_none(),
        "conversation must not leak across workspaces"
    );
}

// 5. from_sqlx / is_unique_violation mapping honesty

#[tokio::test]
async fn from_sqlx_maps_23505_to_conflict_and_is_unique_violation() {
    // Direct mapping unit-style (no DB needed for the pure path), but we still
    // require DATABASE_URL so this file stays fail-closed as a suite, and we
    // also exercise a live 23505 through map_query_err below.
    let _pool = connect_and_migrate().await;

    // Construct a synthetic Database error is awkward without a live driver;
    // assert the public contract via a real unique violation (workspace slug)
    // and via Conflict variants used by the crate.
    let generic = DbError::Conflict("unique constraint violated".into());
    assert!(
        generic.is_unique_violation(),
        "from_sqlx produces Conflict(\"unique constraint violated\") for 23505; \
         is_unique_violation must return true for that mapped form"
    );

    let domain = DbError::Conflict("an account with this email already exists".into());
    assert!(
        domain.is_unique_violation(),
        "domain-specific unique Conflict messages must also be is_unique_violation"
    );

    // Non-conflict errors are not unique violations.
    let not_found = DbError::NotFound("nope".into());
    assert!(!not_found.is_unique_violation());
}

#[tokio::test]
async fn live_unique_violation_through_map_query_err_is_unique() {
    // Live path: repos use map_query_err → from_sqlx → Conflict.
    // Callers (workspaces route, personas route) gate on is_unique_violation().
    let pool = connect_and_migrate().await;
    let email = unique_email();
    let user_id = seed_user(&pool, &email).await;
    let slug = unique_slug("map");
    let now = Utc::now();

    workspaces::create_owned(&pool, Uuid::now_v7(), user_id, &slug, "A", now)
        .await
        .expect("first");

    let err = workspaces::create_owned(&pool, Uuid::now_v7(), user_id, &slug, "B", now)
        .await
        .expect_err("second slug");

    assert!(
        matches!(&err, DbError::Conflict(m) if m == "unique constraint violated"),
        "live 23505 must become Conflict(unique constraint violated), got {err:?}"
    );
    assert!(
        err.is_unique_violation(),
        "is_unique_violation must be true after from_sqlx mapping — otherwise \
         fleet-auth workspaces/personas custom conflict messages never fire"
    );
}

#[tokio::test]
async fn ticket_monitor_and_inbox_upsert_and_launch() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;

    let mon = tickets::upsert_monitor(
        &pool,
        &tickets::UpsertMonitor {
            workspace_id,
            enabled: true,
            providers: vec!["linear".into(), "jira".into()],
            github_repos: vec![],
            jira_jql: None,
            slack_channels: vec![],
            notion_databases: vec![],
            auto_launch: false,
            follow_connected: true,
            filters: serde_json::json!({}),
            launch_limit: 8,
            poll_limit: 20,
            hook_token: format!("tok-monitor-1-{}", Uuid::now_v7()),
            started_by: Some(user_id),
        },
    )
    .await
    .expect("upsert monitor");
    assert!(mon.enabled);
    assert_eq!(mon.providers.len(), 2);

    let enabled = tickets::list_enabled_workspace_ids(&pool)
        .await
        .expect("enabled");
    assert!(enabled.contains(&workspace_id));

    let first = tickets::upsert_inbox(
        &pool,
        &tickets::UpsertInbox {
            workspace_id,
            provider: "linear".into(),
            external_key: "ENG-1".into(),
            title: "One".into(),
            url: None,
            summary: "first".into(),
            provider_closed: false,
        },
    )
    .await
    .expect("inbox 1");
    assert_eq!(first.state, "open");

    let again = tickets::upsert_inbox(
        &pool,
        &tickets::UpsertInbox {
            workspace_id,
            provider: "linear".into(),
            external_key: "ENG-1".into(),
            title: "One updated".into(),
            url: None,
            summary: "second".into(),
            provider_closed: false,
        },
    )
    .await
    .expect("inbox upsert");
    assert_eq!(again.title, "One updated");
    assert_eq!(again.summary, "second");
    assert_eq!(again.state, "open");

    let run = Uuid::now_v7();
    let launched = tickets::mark_launched(&pool, workspace_id, "linear", "ENG-1", run)
        .await
        .expect("launch")
        .expect("row");
    assert_eq!(launched.state, "launched");
    assert_eq!(launched.run_id, Some(run));

    let after_close = tickets::upsert_inbox(
        &pool,
        &tickets::UpsertInbox {
            workspace_id,
            provider: "linear".into(),
            external_key: "ENG-1".into(),
            title: "One closed".into(),
            url: None,
            summary: "should keep launched summary".into(),
            provider_closed: true,
        },
    )
    .await
    .expect("closed upsert");
    assert_eq!(
        after_close.state, "done",
        "closed on the board while launched must mark the inbox row done"
    );
    assert_eq!(after_close.summary, "second");
}

#[tokio::test]
async fn claim_launch_blocks_a_second_claim() {
    let pool = connect_and_migrate().await;
    let (_user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    tickets::upsert_inbox(
        &pool,
        &tickets::UpsertInbox {
            workspace_id,
            provider: "github".into(),
            external_key: "acme/app#1".into(),
            title: "One".into(),
            url: None,
            summary: "s".into(),
            provider_closed: false,
        },
    )
    .await
    .expect("upsert");
    let first = tickets::claim_launch(&pool, workspace_id, "github", "acme/app#1")
        .await
        .expect("claim")
        .expect("row");
    assert_eq!(first.state, "launched");
    assert!(first.run_id.is_none());
    assert!(
        tickets::claim_launch(&pool, workspace_id, "github", "acme/app#1")
            .await
            .expect("second")
            .is_none()
    );
    let run = Uuid::now_v7();
    let attached = tickets::attach_launch_run(&pool, workspace_id, "github", "acme/app#1", run)
        .await
        .expect("attach")
        .expect("row");
    assert_eq!(attached.run_id, Some(run));
}

#[tokio::test]
async fn ticket_reopen_after_failed_run_and_mark_done() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let _ = tickets::upsert_monitor(
        &pool,
        &tickets::UpsertMonitor {
            workspace_id,
            enabled: true,
            providers: vec!["jira".into()],
            github_repos: vec![],
            jira_jql: Some("project = ENG AND status != Done".into()),
            slack_channels: vec![],
            notion_databases: vec![],
            auto_launch: true,
            follow_connected: true,
            filters: serde_json::json!({"jira": {"jql": "project = ENG AND status != Done"}}),
            launch_limit: 8,
            poll_limit: 20,
            hook_token: format!("tok-monitor-2-{}", Uuid::now_v7()),
            started_by: Some(user_id),
        },
    )
    .await
    .expect("monitor");

    let item = tickets::upsert_inbox(
        &pool,
        &tickets::UpsertInbox {
            workspace_id,
            provider: "jira".into(),
            external_key: "ENG-9".into(),
            title: "Broken login".into(),
            url: None,
            summary: "fix it".into(),
            provider_closed: false,
        },
    )
    .await
    .expect("inbox");
    assert_eq!(item.state, "open");
    assert!(item.run_id.is_none());

    let run = Uuid::now_v7();
    tickets::mark_launched(&pool, workspace_id, "jira", "ENG-9", run)
        .await
        .expect("launch")
        .expect("row");

    let reopened = tickets::reopen_ticket(&pool, workspace_id, "jira", "ENG-9")
        .await
        .expect("reopen")
        .expect("row");
    assert_eq!(reopened.state, "open");
    assert!(reopened.run_id.is_none(), "failed run must clear run_id");

    tickets::mark_launched(&pool, workspace_id, "jira", "ENG-9", Uuid::now_v7())
        .await
        .expect("relaunch");
    let done = tickets::mark_inbox_done(&pool, workspace_id, "jira", "ENG-9")
        .await
        .expect("done")
        .expect("row");
    assert_eq!(done.state, "done");
}

// cases: create → claim wake → patch complete

#[tokio::test]
async fn case_create_claim_and_complete() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let id = Uuid::now_v7();
    let past = Utc::now() - Duration::seconds(5);
    let case = cases::insert(
        &pool,
        &cases::NewCase {
            id,
            workspace_id,
            kind: "outreach".into(),
            title: "Reply watch".into(),
            policy_md: "If observed reply, wait for confirm; then complete.".into(),
            stage: "start".into(),
            context: json!({}),
            next_wake_at: Some(past),
            created_by: Some(user_id),
        },
    )
    .await
    .expect("insert case");
    assert_eq!(case.status, "open");
    assert_eq!(case.stage, "start");

    let _ = cases::append_event(&pool, id, workspace_id, "created", json!({ "ok": true }))
        .await
        .expect("event");

    // claim_due is global (oldest due first, limit 10). A dirty shared
    // Postgres can have leftover due rows from earlier runs.
    let mut claimed_row = None;
    for _ in 0..80 {
        let batch = cases::claim_due(&pool, 50).await.expect("claim");
        if batch.is_empty() {
            break;
        }
        if let Some(c) = batch.into_iter().find(|c| c.id == id) {
            claimed_row = Some(c);
            break;
        }
    }
    let claimed_row = claimed_row.expect("case should be claimed when next_wake_at is past");
    assert_eq!(claimed_row.status, "deciding");

    let done = cases::apply_patch(
        &pool,
        workspace_id,
        id,
        &cases::CasePatch {
            status: "completed".into(),
            stage: "booked".into(),
            context: json!({ "force_complete": true }),
            next_wake_at: None,
            last_error: None,
            run_id: None,
            clear_run_id: false,
            bump_decision: true,
            expected_version: claimed_row.version,
        },
    )
    .await
    .expect("complete");
    assert_eq!(done.status, "completed");
    assert_eq!(done.stage, "booked");
    assert_eq!(done.decision_count, 1);

    let listed = cases::list_for_workspace(&pool, workspace_id, Some("completed"), 20)
        .await
        .expect("list");
    assert!(listed.iter().any(|c| c.id == id));

    let events = cases::list_events(&pool, id, 10).await.expect("events");
    assert!(!events.is_empty());
}

// ---------------------------------------------------------------------------
// partners disclaimer: default NULL, first write wins, hosts unchanged
// ---------------------------------------------------------------------------

/// Identity of a worker host that must not change when the disclaimer is ack'd.
fn host_fingerprint(h: &fleet_db::models::WorkerHost) -> (Uuid, &str, bool, bool, bool, &str, i32) {
    (
        h.id,
        h.status.as_str(),
        h.enabled,
        h.paused,
        h.drain,
        h.isolation.as_str(),
        h.current_jobs,
    )
}

#[tokio::test]
async fn partners_disclaimer_defaults_null_and_first_write_wins() {
    let pool = connect_and_migrate().await;
    let (_user_id, workspace_id) = seed_user_with_workspace(&pool).await;

    let raw: Option<chrono::DateTime<Utc>> =
        sqlx::query_scalar("SELECT partners_disclaimer_accepted_at FROM workspaces WHERE id = $1")
            .bind(workspace_id)
            .fetch_one(&pool)
            .await
            .expect("raw accepted_at");
    assert!(
        raw.is_none(),
        "new workspace column default must be NULL, got {raw:?}"
    );

    let unread = workspaces::partners_disclaimer_accepted_at(&pool, workspace_id)
        .await
        .expect("read default");
    assert_eq!(unread, None, "repo must treat NULL as not accepted");
    assert!(
        !PartnersDisclaimerAck::from_accepted_at(unread).accepted,
        "PartnersDisclaimerAck must report not accepted when timestamp is None"
    );

    let first_at = Utc::now().trunc_subsecs(6);
    let stored = workspaces::accept_partners_disclaimer(&pool, workspace_id, first_at)
        .await
        .expect("first accept");
    assert_eq!(stored, first_at, "accept must write the provided timestamp");

    let after_first = workspaces::partners_disclaimer_accepted_at(&pool, workspace_id)
        .await
        .expect("read after first accept");
    assert_eq!(after_first, Some(first_at));
    assert!(PartnersDisclaimerAck::from_accepted_at(after_first).accepted);

    let later = first_at + Duration::hours(1);
    let again = workspaces::accept_partners_disclaimer(&pool, workspace_id, later)
        .await
        .expect("second accept");
    assert_eq!(
        again, first_at,
        "COALESCE must keep the original timestamp (first write wins)"
    );

    let after_second = workspaces::partners_disclaimer_accepted_at(&pool, workspace_id)
        .await
        .expect("read after second accept");
    assert_eq!(after_second, Some(first_at));
}

#[tokio::test]
async fn accept_partners_disclaimer_does_not_change_worker_hosts_or_assignable() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let expires = Utc::now() + Duration::hours(1);

    let pending_token = worker_hosts::generate_pair_token();
    let pending = worker_hosts::create_pending(
        &pool,
        &NewWorkerHost::new(Uuid::now_v7(), workspace_id, "pending-box", Some(user_id)),
        &worker_hosts::hash_token(&pending_token),
        expires,
    )
    .await
    .expect("create pending host");

    let online_token = worker_hosts::generate_pair_token();
    let online = worker_hosts::create_pending(
        &pool,
        &NewWorkerHost::new(Uuid::now_v7(), workspace_id, "online-box", Some(user_id)),
        &worker_hosts::hash_token(&online_token),
        expires,
    )
    .await
    .expect("create online host");
    worker_hosts::consume_pair_token(
        &pool,
        &online_token,
        "online-box",
        "linux-x86_64",
        true,
        false,
        None,
    )
    .await
    .expect("pair online host");

    let hosts_before = worker_hosts::list_for_workspace(&pool, workspace_id)
        .await
        .expect("list hosts before");
    let assignable_before =
        worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
            .await
            .expect("list assignable before");
    let host_ids_before: Vec<Uuid> = hosts_before.iter().map(|h| h.id).collect();
    let assignable_ids_before: Vec<Uuid> = assignable_before.iter().map(|h| h.id).collect();
    assert!(host_ids_before.contains(&pending.id));
    assert!(host_ids_before.contains(&online.id));
    assert!(
        assignable_ids_before.contains(&online.id),
        "paired online host must be assignable before accept: {assignable_ids_before:?}"
    );
    assert!(
        !assignable_ids_before.contains(&pending.id),
        "pending host must not be assignable: {assignable_ids_before:?}"
    );

    workspaces::accept_partners_disclaimer(&pool, workspace_id, Utc::now())
        .await
        .expect("accept disclaimer");

    let hosts_after = worker_hosts::list_for_workspace(&pool, workspace_id)
        .await
        .expect("list hosts after");
    let assignable_after =
        worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
            .await
            .expect("list assignable after");

    assert_eq!(
        hosts_after.iter().map(host_fingerprint).collect::<Vec<_>>(),
        hosts_before
            .iter()
            .map(host_fingerprint)
            .collect::<Vec<_>>(),
        "accept must not create, drop, or mutate worker_hosts"
    );
    assert_eq!(
        assignable_after.iter().map(|h| h.id).collect::<Vec<_>>(),
        assignable_ids_before,
        "accept must not change assignable hosts"
    );
    assert!(
        workspaces::partners_disclaimer_accepted_at(&pool, workspace_id)
            .await
            .expect("read accepted_at")
            .is_some(),
        "disclaimer must still have been recorded"
    );
}

// ---------------------------------------------------------------------------
// workspace min_isolation: default process, store wire strings only
// ---------------------------------------------------------------------------

#[tokio::test]
async fn workspace_min_isolation_defaults_process_and_stores_wire_strings() {
    let pool = connect_and_migrate().await;
    let (_user_id, workspace_id) = seed_user_with_workspace(&pool).await;

    let raw: String = sqlx::query_scalar("SELECT min_isolation FROM workspaces WHERE id = $1")
        .bind(workspace_id)
        .fetch_one(&pool)
        .await
        .expect("raw min_isolation");
    assert_eq!(raw, "process", "new workspace must default to process");

    let via_repo = workspaces::min_isolation(&pool, workspace_id)
        .await
        .expect("repo default");
    assert_eq!(via_repo, "process");

    let container = workspaces::set_min_isolation(&pool, workspace_id, "container")
        .await
        .expect("set container");
    assert_eq!(container, "container");
    assert_eq!(
        workspaces::min_isolation(&pool, workspace_id)
            .await
            .expect("read container"),
        "container"
    );

    let vm = workspaces::set_min_isolation(&pool, workspace_id, " vm ")
        .await
        .expect("set vm");
    assert_eq!(vm, "vm");

    let err = workspaces::set_min_isolation(&pool, workspace_id, "native")
        .await
        .expect_err("native is not a stored IsolationClass string");
    match &err {
        DbError::Conflict(msg) => {
            assert_eq!(msg, "min_isolation must be process, container, or vm");
        }
        other => panic!("expected Conflict for unknown min_isolation, got {other:?}"),
    }
    assert_eq!(
        workspaces::min_isolation(&pool, workspace_id)
            .await
            .expect("unchanged after reject"),
        "vm"
    );

    let missing = workspaces::min_isolation(&pool, Uuid::now_v7())
        .await
        .expect_err("missing workspace");
    assert!(matches!(missing, DbError::NotFound(_)));
}
