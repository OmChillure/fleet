//! Fail-closed Postgres repository tests for fleet-db.
//! Requires DATABASE_URL — never skips.
//!
//! Criticizes expected behavior at the repo boundary (not HTTP).

mod common;

use chrono::{Duration, Utc};
use fleet_db::error::DbError;
use fleet_db::models::NewAuthSession;
use fleet_db::repos::{cases, chats, sessions, tickets, users, workspaces};
use serde_json::json;
use fleet_db::repos::users::NewUser;
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

    sessions::insert(&pool, &new)
        .await
        .expect("insert session");

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
    assert_eq!(after_close.state, "launched");
    assert_eq!(after_close.summary, "second");
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

    let _ = cases::append_event(
        &pool,
        id,
        workspace_id,
        "created",
        json!({ "ok": true }),
    )
    .await
    .expect("event");

    let claimed = cases::claim_due(&pool, 10).await.expect("claim");
    assert!(
        claimed.iter().any(|c| c.id == id),
        "case should be claimed when next_wake_at is past"
    );
    let claimed_row = claimed.into_iter().find(|c| c.id == id).unwrap();
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
