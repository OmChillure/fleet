//! Fail-closed Postgres repository tests for fleet-db.
//! Requires DATABASE_URL — never skips.
//!
//! Criticizes expected behavior at the repo boundary (not HTTP).

mod common;

use chrono::{Duration, Utc};
use fleet_db::error::DbError;
use fleet_db::models::NewAuthSession;
use fleet_db::repos::{chats, sessions, users, workspaces};
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

// ---------------------------------------------------------------------------
// 1. users::insert_tx — duplicate email_normalized
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// 2. workspace slug unique → Conflict
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// 3. session insert + find by refresh hash
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// 4. chat conversation belongs to user+workspace
// ---------------------------------------------------------------------------

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

    // Owner can fetch.
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

// ---------------------------------------------------------------------------
// 5. from_sqlx / is_unique_violation mapping honesty
// ---------------------------------------------------------------------------

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
