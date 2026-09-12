//! Fail-closed Postgres repository tests for fleet-db.
//! Requires DATABASE_URL — never skips.
//!
//! Criticizes expected behavior at the repo boundary (not HTTP).

mod common;

use chrono::{Duration, SubsecRound, Utc};
use fleet_db::error::DbError;
use fleet_db::models::{NewAuthSession, NewWorkerHost};
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
    let first_id = Uuid::now_v7();

    let mut tx = pool.begin().await.expect("begin first");
    users::insert_tx(
        &mut tx,
        &NewUser {
            id: first_id,
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

    let mut tx = pool.begin().await.expect("begin second");
    let err = users::insert_tx(
        &mut tx,
        &NewUser {
            id: Uuid::now_v7(),
            email,
            email_normalized: normalized.clone(),
            display_name: Some("Second".into()),
            password_hash: "hash-b".into(),
            created_at: now,
        },
    )
    .await
    .expect_err("duplicate email_normalized must fail");
    let _ = tx.rollback().await;

    match &err {
        DbError::Conflict(_) => {}
        other => panic!("expected DbError::Conflict, got {other:?}"),
    }

    let leftover = users::find_by_email_normalized(&pool, &normalized)
        .await
        .expect("find leftover by email_normalized");
    assert_eq!(
        leftover.map(|u| u.id),
        Some(first_id),
        "after Conflict, find_by_email_normalized must return the first id only"
    );
}

#[tokio::test]
async fn insert_tx_duplicate_email_conflicts_when_first_user_is_disabled() {
    let pool = connect_and_migrate().await;
    let email = unique_email();
    let normalized = email.to_lowercase();
    let first_id = seed_user(&pool, &email).await;

    // users_email_normalized_uidx is not partial on status.
    let n = sqlx::query("UPDATE users SET status = 'disabled' WHERE id = $1")
        .bind(first_id)
        .execute(&pool)
        .await
        .expect("disable first user");
    assert_eq!(n.rows_affected(), 1, "expected one user row updated");

    let mut tx = pool.begin().await.expect("begin second");
    let err = users::insert_tx(
        &mut tx,
        &NewUser {
            id: Uuid::now_v7(),
            email,
            email_normalized: normalized,
            display_name: Some("Second".into()),
            password_hash: "hash-b".into(),
            created_at: Utc::now(),
        },
    )
    .await
    .expect_err("duplicate email_normalized must fail when first user is disabled");
    let _ = tx.rollback().await;

    match &err {
        DbError::Conflict(_) => {}
        other => panic!(
            "expected DbError::Conflict when first user is disabled, got {other:?}"
        ),
    }
}

// 2. workspace slug unique → Conflict

#[tokio::test]
async fn workspace_slug_unique_is_conflict() {
    let pool = connect_and_migrate().await;
    let email = unique_email();
    let user_id = seed_user(&pool, &email).await;
    let slug = unique_slug("shared");
    let now = Utc::now();
    let first_id = Uuid::now_v7();

    workspaces::create_owned(&pool, first_id, user_id, &slug, "One", now)
        .await
        .expect("first workspace");

    // Second owner on same slug — unique index workspaces_slug_uidx.
    let email2 = unique_email();
    let user2 = seed_user(&pool, &email2).await;
    let err = workspaces::create_owned(&pool, Uuid::now_v7(), user2, &slug, "Two", now)
        .await
        .expect_err("duplicate slug must fail");

    match &err {
        DbError::Conflict(_) => {}
        other => panic!("expected DbError::Conflict from unique slug, got {other:?}"),
    }
    assert!(
        err.is_unique_violation(),
        "mapped Conflict from 23505 must be is_unique_violation (workspaces route relies on this)"
    );

    let leftover = workspaces::find_for_user(&pool, first_id, user_id)
        .await
        .expect("find leftover workspace");
    assert_eq!(
        leftover.as_ref().map(|w| (w.id, w.slug.as_str())),
        Some((first_id, slug.as_str())),
        "after Conflict, find_for_user must return the first workspace with that slug"
    );
    let second_role = workspaces::user_role(&pool, first_id, user2)
        .await
        .expect("user_role for second owner");
    assert_eq!(
        second_role,
        None,
        "second owner must not gain membership on the first workspace"
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

#[tokio::test]
async fn ticket_monitor_and_inbox_upsert_and_launch() {
    let pool = connect_and_migrate().await;
    let (_user_id, workspace_id) = seed_user_with_workspace(&pool).await;

    tickets::upsert_inbox(
        &pool,
        &tickets::UpsertInbox {
            workspace_id,
            provider: "linear".into(),
            external_key: "ENG-1".into(),
            title: "One".into(),
            url: None,
            summary: "launched summary".into(),
            provider_closed: false,
        },
    )
    .await
    .expect("inbox");

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
    assert_eq!(after_close.summary, "launched summary");
    assert_eq!(
        after_close.run_id,
        Some(run),
        "closing a launched row must keep the attached run_id"
    );
}

#[tokio::test]
async fn upsert_inbox_open_repoll_keeps_launched_state_and_summary() {
    let pool = connect_and_migrate().await;
    let (_user_id, workspace_id) = seed_user_with_workspace(&pool).await;

    tickets::upsert_inbox(
        &pool,
        &tickets::UpsertInbox {
            workspace_id,
            provider: "linear".into(),
            external_key: "ENG-2".into(),
            title: "Open".into(),
            url: None,
            summary: "frozen".into(),
            provider_closed: false,
        },
    )
    .await
    .expect("inbox");

    let run = Uuid::now_v7();
    tickets::mark_launched(&pool, workspace_id, "linear", "ENG-2", run)
        .await
        .expect("launch")
        .expect("row");

    let again = tickets::upsert_inbox(
        &pool,
        &tickets::UpsertInbox {
            workspace_id,
            provider: "linear".into(),
            external_key: "ENG-2".into(),
            title: "Open updated".into(),
            url: None,
            summary: "must not replace launched summary".into(),
            provider_closed: false,
        },
    )
    .await
    .expect("open repoll");
    assert_eq!(again.state, "launched");
    assert_eq!(again.run_id, Some(run));
    assert_eq!(again.summary, "frozen");
    assert_eq!(again.title, "Open updated");
}

#[tokio::test]
async fn list_enabled_workspace_ids_omits_disabled() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;

    tickets::upsert_monitor(
        &pool,
        &tickets::UpsertMonitor {
            workspace_id,
            enabled: false,
            providers: vec!["linear".into()],
            github_repos: vec![],
            jira_jql: None,
            slack_channels: vec![],
            notion_databases: vec![],
            auto_launch: false,
            follow_connected: true,
            filters: serde_json::json!({}),
            launch_limit: 8,
            poll_limit: 20,
            hook_token: format!("tok-disabled-{}", Uuid::now_v7()),
            started_by: Some(user_id),
        },
    )
    .await
    .expect("disabled monitor");

    let enabled = tickets::list_enabled_workspace_ids(&pool)
        .await
        .expect("enabled");
    assert!(
        !enabled.contains(&workspace_id),
        "disabled monitor must not appear in list_enabled_workspace_ids"
    );
}

#[tokio::test]
async fn upsert_monitor_conflict_keeps_existing_hook_token() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let original = format!("tok-keep-{}", Uuid::now_v7());

    tickets::upsert_monitor(
        &pool,
        &tickets::UpsertMonitor {
            workspace_id,
            enabled: true,
            providers: vec!["linear".into()],
            github_repos: vec![],
            jira_jql: None,
            slack_channels: vec![],
            notion_databases: vec![],
            auto_launch: false,
            follow_connected: true,
            filters: serde_json::json!({}),
            launch_limit: 8,
            poll_limit: 20,
            hook_token: original.clone(),
            started_by: Some(user_id),
        },
    )
    .await
    .expect("first monitor");

    let again = tickets::upsert_monitor(
        &pool,
        &tickets::UpsertMonitor {
            workspace_id,
            enabled: true,
            providers: vec!["jira".into()],
            github_repos: vec![],
            jira_jql: None,
            slack_channels: vec![],
            notion_databases: vec![],
            auto_launch: false,
            follow_connected: true,
            filters: serde_json::json!({}),
            launch_limit: 8,
            poll_limit: 20,
            hook_token: format!("tok-rotate-{}", Uuid::now_v7()),
            started_by: Some(user_id),
        },
    )
    .await
    .expect("second monitor");
    assert_eq!(
        again.hook_token, original,
        "ON CONFLICT must COALESCE hook_token so the webhook URL does not rotate"
    );
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
            .is_none(),
        "claim_launch must return None when the row is already launched so a second Finish cannot insert another run"
    );
    let after = tickets::find_inbox(&pool, workspace_id, "github", "acme/app#1")
        .await
        .expect("re-read")
        .expect("row still exists");
    assert_eq!(after.state, "launched");
    assert!(
        after.run_id.is_none(),
        "a blocked second claim must leave the launched row with run_id still null"
    );
}

#[tokio::test]
async fn attach_launch_run_blocks_a_second_attach() {
    let pool = connect_and_migrate().await;
    let (_user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    tickets::upsert_inbox(
        &pool,
        &tickets::UpsertInbox {
            workspace_id,
            provider: "github".into(),
            external_key: "acme/app#2".into(),
            title: "Two".into(),
            url: None,
            summary: "s".into(),
            provider_closed: false,
        },
    )
    .await
    .expect("upsert");
    tickets::claim_launch(&pool, workspace_id, "github", "acme/app#2")
        .await
        .expect("claim")
        .expect("row");
    let run = Uuid::now_v7();
    let attached = tickets::attach_launch_run(&pool, workspace_id, "github", "acme/app#2", run)
        .await
        .expect("attach")
        .expect("row");
    assert_eq!(attached.run_id, Some(run));
    let other = Uuid::now_v7();
    assert!(
        tickets::attach_launch_run(&pool, workspace_id, "github", "acme/app#2", other)
            .await
            .expect("second attach")
            .is_none(),
        "attach_launch_run must return None when run_id is already set"
    );
}

#[tokio::test]
async fn attach_launch_run_on_open_row_is_none() {
    let pool = connect_and_migrate().await;
    let (_user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    tickets::upsert_inbox(
        &pool,
        &tickets::UpsertInbox {
            workspace_id,
            provider: "github".into(),
            external_key: "acme/app#3".into(),
            title: "Three".into(),
            url: None,
            summary: "s".into(),
            provider_closed: false,
        },
    )
    .await
    .expect("upsert");
    let run = Uuid::now_v7();
    assert!(
        tickets::attach_launch_run(&pool, workspace_id, "github", "acme/app#3", run)
            .await
            .expect("attach without claim")
            .is_none(),
        "attach_launch_run must return None unless the row is already launched"
    );
}

#[tokio::test]
async fn ticket_reopen_after_failed_run_and_mark_done() {
    let pool = connect_and_migrate().await;
    let (_user_id, workspace_id) = seed_user_with_workspace(&pool).await;

    tickets::upsert_inbox(
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

    let run = Uuid::now_v7();
    let launched = tickets::mark_launched(&pool, workspace_id, "jira", "ENG-9", run)
        .await
        .expect("launch")
        .expect("row");
    assert_eq!(launched.state, "launched");
    assert_eq!(launched.run_id, Some(run));

    let reopened = tickets::reopen_ticket(&pool, workspace_id, "jira", "ENG-9")
        .await
        .expect("reopen")
        .expect("row");
    assert_eq!(reopened.state, "open");
    assert!(reopened.run_id.is_none(), "failed run must clear run_id");

    let retry = Uuid::now_v7();
    let relaunched = tickets::mark_launched(&pool, workspace_id, "jira", "ENG-9", retry)
        .await
        .expect("relaunch")
        .expect("retry claim must succeed after reopen cleared run_id");
    assert_eq!(relaunched.state, "launched");
    assert_eq!(relaunched.run_id, Some(retry));
}

#[tokio::test]
async fn reopen_tickets_for_run_clears_launched_row() {
    let pool = connect_and_migrate().await;
    let (_user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    tickets::upsert_inbox(
        &pool,
        &tickets::UpsertInbox {
            workspace_id,
            provider: "jira".into(),
            external_key: "E2E-1".into(),
            title: "cancel me".into(),
            url: None,
            summary: "x".into(),
            provider_closed: false,
        },
    )
    .await
    .expect("inbox");
    let run = Uuid::now_v7();
    tickets::mark_launched(&pool, workspace_id, "jira", "E2E-1", run)
        .await
        .expect("launch")
        .expect("row");
    let n = tickets::reopen_tickets_for_run(&pool, workspace_id, run)
        .await
        .expect("reopen by run");
    assert_eq!(n, 1);
    let row = tickets::find_inbox(&pool, workspace_id, "jira", "E2E-1")
        .await
        .expect("find")
        .expect("row");
    assert_eq!(row.state, "open");
    assert!(row.run_id.is_none());
}

#[tokio::test]
async fn mark_inbox_done_sets_state_done_and_keeps_run_id() {
    let pool = connect_and_migrate().await;
    let (_user_id, workspace_id) = seed_user_with_workspace(&pool).await;

    tickets::upsert_inbox(
        &pool,
        &tickets::UpsertInbox {
            workspace_id,
            provider: "jira".into(),
            external_key: "ENG-10".into(),
            title: "Finished".into(),
            url: None,
            summary: "done".into(),
            provider_closed: false,
        },
    )
    .await
    .expect("inbox");

    let run = Uuid::now_v7();
    tickets::mark_launched(&pool, workspace_id, "jira", "ENG-10", run)
        .await
        .expect("launch")
        .expect("row");

    let done = tickets::mark_inbox_done(&pool, workspace_id, "jira", "ENG-10")
        .await
        .expect("done")
        .expect("row");
    assert_eq!(done.state, "done");
    assert_eq!(
        done.run_id,
        Some(run),
        "mark_inbox_done must SET state=done without clearing run_id"
    );
}

#[tokio::test]
async fn reopen_ticket_on_ignored_returns_none_and_leaves_state() {
    let pool = connect_and_migrate().await;
    let (_user_id, workspace_id) = seed_user_with_workspace(&pool).await;

    tickets::upsert_inbox(
        &pool,
        &tickets::UpsertInbox {
            workspace_id,
            provider: "jira".into(),
            external_key: "ENG-11".into(),
            title: "Skip".into(),
            url: None,
            summary: "ignored".into(),
            provider_closed: false,
        },
    )
    .await
    .expect("inbox");

    let run = Uuid::now_v7();
    tickets::mark_launched(&pool, workspace_id, "jira", "ENG-11", run)
        .await
        .expect("launch")
        .expect("row");
    let n = sqlx::query(
        "UPDATE ticket_inbox SET state = 'ignored' WHERE workspace_id = $1 AND provider = $2 AND external_key = $3",
    )
    .bind(workspace_id)
    .bind("jira")
    .bind("ENG-11")
    .execute(&pool)
    .await
    .expect("force ignored");
    assert_eq!(n.rows_affected(), 1, "expected one inbox row forced to ignored");

    assert!(
        tickets::reopen_ticket(&pool, workspace_id, "jira", "ENG-11")
            .await
            .expect("reopen")
            .is_none(),
        "reopen_ticket must return None when state is ignored (not launched/done)"
    );

    let row = tickets::find_inbox(&pool, workspace_id, "jira", "ENG-11")
        .await
        .expect("re-read")
        .expect("row");
    assert_eq!(row.state, "ignored");
    assert_eq!(
        row.run_id,
        Some(run),
        "ignored row run_id must stay attached when reopen matches nothing"
    );
}

#[tokio::test]
async fn reopen_ticket_wrong_workspace_returns_none() {
    let pool = connect_and_migrate().await;
    let (_user_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (_user_b, ws_b) = seed_user_with_workspace(&pool).await;

    tickets::upsert_inbox(
        &pool,
        &tickets::UpsertInbox {
            workspace_id: ws_a,
            provider: "jira".into(),
            external_key: "ENG-12".into(),
            title: "A".into(),
            url: None,
            summary: "s".into(),
            provider_closed: false,
        },
    )
    .await
    .expect("inbox");

    let run = Uuid::now_v7();
    tickets::mark_launched(&pool, ws_a, "jira", "ENG-12", run)
        .await
        .expect("launch")
        .expect("row");

    assert!(
        tickets::reopen_ticket(&pool, ws_b, "jira", "ENG-12")
            .await
            .expect("reopen other workspace")
            .is_none(),
        "reopen_ticket must return None when workspace_id does not match"
    );

    let row = tickets::find_inbox(&pool, ws_a, "jira", "ENG-12")
        .await
        .expect("re-read")
        .expect("row");
    assert_eq!(row.state, "launched");
    assert_eq!(
        row.run_id,
        Some(run),
        "launched row in the owning workspace must keep run_id when reopen is scoped to another workspace"
    );
}

// cases: insert defaults open; claim_due flips open+due → deciding

/// `claim_due` is process-global + `SKIP LOCKED`. Drive until `id` is
/// deciding on a fresh `get()`, not until this process's batch contains it.
async fn claim_due_until_deciding(pool: &fleet_db::PgPool, workspace_id: Uuid, id: Uuid) {
    for _ in 0..16 {
        let batch = cases::claim_due(pool, 50).await.expect("claim_due");
        let row = cases::get(pool, workspace_id, id)
            .await
            .expect("get during claim")
            .expect("row");
        if row.status == "deciding" {
            return;
        }
        if batch.is_empty() {
            tokio::task::yield_now().await;
        }
    }
}

#[tokio::test]
async fn case_create_claim_and_complete() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let id = Uuid::now_v7();
    // Oldest wake so leftover due rows cannot sort ahead of this open row.
    let due = chrono::DateTime::<Utc>::UNIX_EPOCH;
    let case = cases::insert(
        &pool,
        &cases::NewCase {
            id,
            workspace_id,
            kind: "generic".into(),
            title: "open due".into(),
            policy_md: String::new(),
            stage: "start".into(),
            context: json!({}),
            next_wake_at: Some(due),
            created_by: Some(user_id),
        },
    )
    .await
    .expect("insert case");
    assert_eq!(
        case.status, "open",
        "insert must default status=open so wake_in_s=0 creates are claimable"
    );
    let version = case.version;

    claim_due_until_deciding(&pool, workspace_id, id).await;

    let after = cases::get(&pool, workspace_id, id)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(
        after.status, "deciding",
        "claim_due must flip open+due to deciding (this process or a concurrent claimer); status={}",
        after.status
    );
    assert!(
        after.version >= version + 1,
        "claim_due must bump version on the open+due row (before={version}, after={})",
        after.version
    );
}

#[tokio::test]
async fn claim_due_skips_open_with_null_next_wake_at() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let idle = cases::insert(
        &pool,
        &cases::NewCase {
            id: Uuid::now_v7(),
            workspace_id,
            kind: "generic".into(),
            title: "open null wake".into(),
            policy_md: String::new(),
            stage: "start".into(),
            context: json!({}),
            next_wake_at: None,
            created_by: Some(user_id),
        },
    )
    .await
    .expect("insert null-wake open");
    assert_eq!(idle.status, "open");
    assert!(idle.next_wake_at.is_none());
    let idle_version = idle.version;

    // Future wake first so a parallel claim_due cannot snatch the open row.
    let control = cases::insert(
        &pool,
        &cases::NewCase {
            id: Uuid::now_v7(),
            workspace_id,
            kind: "generic".into(),
            title: "waiting due control".into(),
            policy_md: String::new(),
            stage: "start".into(),
            context: json!({}),
            next_wake_at: Some(Utc::now() + Duration::hours(1)),
            created_by: Some(user_id),
        },
    )
    .await
    .expect("insert waiting control");
    let control = cases::apply_patch(
        &pool,
        workspace_id,
        control.id,
        &cases::CasePatch {
            status: "waiting".into(),
            stage: "start".into(),
            context: json!({}),
            next_wake_at: Some(chrono::DateTime::<Utc>::UNIX_EPOCH),
            last_error: None,
            run_id: None,
            clear_run_id: false,
            bump_decision: false,
            expected_version: control.version,
        },
    )
    .await
    .expect("patch waiting due");
    assert_eq!(control.status, "waiting");

    claim_due_until_deciding(&pool, workspace_id, control.id).await;

    let control_after = cases::get(&pool, workspace_id, control.id)
        .await
        .expect("get control")
        .expect("control row");
    assert_eq!(
        control_after.status, "deciding",
        "waiting+due control must be deciding after claim_due; status={}",
        control_after.status
    );

    let idle_after = cases::get(&pool, workspace_id, idle.id)
        .await
        .expect("get idle")
        .expect("idle row");
    assert_eq!(
        idle_after.status, "open",
        "open + next_wake_at NULL must stay open (claim_due requires next_wake_at IS NOT NULL)"
    );
    assert_eq!(
        idle_after.version, idle_version,
        "null-wake open row must not be version-bumped"
    );
    assert!(
        idle_after.next_wake_at.is_none(),
        "null-wake open row must keep next_wake_at NULL"
    );
}

#[tokio::test]
async fn list_for_workspace_does_not_return_other_workspace_cases() {
    let pool = connect_and_migrate().await;
    let (user_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (user_b, ws_b) = seed_user_with_workspace(&pool).await;
    let future = Utc::now() + Duration::hours(1);

    let case_b = cases::insert(
        &pool,
        &cases::NewCase {
            id: Uuid::now_v7(),
            workspace_id: ws_b,
            kind: "generic".into(),
            title: "ws b completed".into(),
            policy_md: String::new(),
            stage: "start".into(),
            context: json!({}),
            next_wake_at: Some(future),
            created_by: Some(user_b),
        },
    )
    .await
    .expect("insert b");
    let case_b = cases::apply_patch(
        &pool,
        ws_b,
        case_b.id,
        &cases::CasePatch {
            status: "completed".into(),
            stage: "start".into(),
            context: json!({}),
            next_wake_at: None,
            last_error: None,
            run_id: None,
            clear_run_id: false,
            bump_decision: false,
            expected_version: case_b.version,
        },
    )
    .await
    .expect("complete b");
    assert_eq!(case_b.status, "completed");

    let case_a = cases::insert(
        &pool,
        &cases::NewCase {
            id: Uuid::now_v7(),
            workspace_id: ws_a,
            kind: "generic".into(),
            title: "ws a completed".into(),
            policy_md: String::new(),
            stage: "start".into(),
            context: json!({}),
            next_wake_at: Some(future),
            created_by: Some(user_a),
        },
    )
    .await
    .expect("insert a");
    let case_a = cases::apply_patch(
        &pool,
        ws_a,
        case_a.id,
        &cases::CasePatch {
            status: "completed".into(),
            stage: "start".into(),
            context: json!({}),
            next_wake_at: None,
            last_error: None,
            run_id: None,
            clear_run_id: false,
            bump_decision: false,
            expected_version: case_a.version,
        },
    )
    .await
    .expect("complete a");
    assert_eq!(case_a.status, "completed");

    let listed = cases::list_for_workspace(&pool, ws_a, Some("completed"), 200)
        .await
        .expect("list");
    assert_eq!(
        listed.iter().map(|c| c.id).collect::<Vec<_>>(),
        vec![case_a.id],
        "list_for_workspace(ws_a, completed) must contain only this workspace's case, not ws_b"
    );
}

#[tokio::test]
async fn append_event_list_events_is_case_scoped_newest_first() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let future = Utc::now() + Duration::hours(1);
    let case_a = cases::insert(
        &pool,
        &cases::NewCase {
            id: Uuid::now_v7(),
            workspace_id,
            kind: "generic".into(),
            title: "case a".into(),
            policy_md: String::new(),
            stage: "start".into(),
            context: json!({}),
            next_wake_at: Some(future),
            created_by: Some(user_id),
        },
    )
    .await
    .expect("insert a");
    let case_b = cases::insert(
        &pool,
        &cases::NewCase {
            id: Uuid::now_v7(),
            workspace_id,
            kind: "generic".into(),
            title: "case b".into(),
            policy_md: String::new(),
            stage: "start".into(),
            context: json!({}),
            next_wake_at: Some(future),
            created_by: Some(user_id),
        },
    )
    .await
    .expect("insert b");

    cases::append_event(&pool, case_a.id, workspace_id, "created", json!({}))
        .await
        .expect("created");
    cases::append_event(&pool, case_a.id, workspace_id, "observed", json!({}))
        .await
        .expect("observed");
    cases::append_event(&pool, case_b.id, workspace_id, "tool", json!({}))
        .await
        .expect("b event");

    let events = cases::list_events(&pool, case_a.id, 10)
        .await
        .expect("list_events");
    assert_eq!(
        events
            .iter()
            .map(|e| (e.case_id, e.kind.as_str()))
            .collect::<Vec<_>>(),
        vec![(case_a.id, "observed"), (case_a.id, "created")],
        "list_events(A) must be newest-first and never include case B"
    );
}

// ---------------------------------------------------------------------------
// partners disclaimer: default NULL, first write wins, hosts unchanged
// ---------------------------------------------------------------------------

/// Identity of a worker host that must not change when the disclaimer is ack'd.
fn host_fingerprint(
    h: &fleet_db::models::WorkerHost,
) -> (Uuid, String, bool, bool, bool, String, i32) {
    (
        h.id,
        h.status.as_str().to_string(),
        h.enabled,
        h.paused,
        h.drain,
        h.isolation.as_str().to_string(),
        h.current_jobs,
    )
}

#[tokio::test]
async fn partners_disclaimer_defaults_null() {
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
        "new workspace column default must be NULL (INSERT omits the column), got {raw:?}"
    );

    let unread = workspaces::partners_disclaimer_accepted_at(&pool, workspace_id)
        .await
        .expect("read default");
    assert_eq!(unread, None, "repo must treat NULL as not accepted");
}

#[tokio::test]
async fn partners_disclaimer_defaults_null_and_first_write_wins() {
    let pool = connect_and_migrate().await;
    let (_user_id, workspace_id) = seed_user_with_workspace(&pool).await;

    let first_at = Utc::now().trunc_subsecs(6);
    let stored = workspaces::accept_partners_disclaimer(&pool, workspace_id, first_at)
        .await
        .expect("first accept");
    assert_eq!(stored, first_at, "accept must write the provided timestamp");

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
async fn accept_partners_disclaimer_missing_workspace_is_not_found() {
    let pool = connect_and_migrate().await;
    let first_at = Utc::now().trunc_subsecs(6);
    let err = workspaces::accept_partners_disclaimer(&pool, Uuid::now_v7(), first_at)
        .await
        .expect_err("missing workspace");
    assert!(
        matches!(err, DbError::NotFound(_)),
        "accept_partners_disclaimer on unknown id must be NotFound, got {err:?}"
    );
}

#[tokio::test]
async fn partners_disclaimer_accepted_at_missing_workspace_is_not_found() {
    let pool = connect_and_migrate().await;
    let err = workspaces::partners_disclaimer_accepted_at(&pool, Uuid::now_v7())
        .await
        .expect_err("missing workspace");
    assert!(
        matches!(err, DbError::NotFound(_)),
        "partners_disclaimer_accepted_at on unknown id must be NotFound, got {err:?}"
    );
}

#[tokio::test]
async fn accept_partners_disclaimer_does_not_write_other_workspace() {
    let pool = connect_and_migrate().await;
    let (_user_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (_user_b, ws_b) = seed_user_with_workspace(&pool).await;

    let first_at = Utc::now().trunc_subsecs(6);
    let stored = workspaces::accept_partners_disclaimer(&pool, ws_a, first_at)
        .await
        .expect("accept a");
    assert_eq!(stored, first_at);

    assert_eq!(
        workspaces::partners_disclaimer_accepted_at(&pool, ws_b)
            .await
            .expect("read b"),
        None,
        "accept(ws_a) must not write ws_b (WHERE id=$1)"
    );
    assert_eq!(
        workspaces::partners_disclaimer_accepted_at(&pool, ws_a)
            .await
            .expect("read a"),
        Some(first_at),
        "accept(ws_a) must keep first_at on ws_a"
    );
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
    let fingerprint_of = |hosts: &[fleet_db::models::WorkerHost], id: Uuid| {
        hosts.iter().find(|h| h.id == id).map(host_fingerprint)
    };
    let pending_fp_before = fingerprint_of(&hosts_before, pending.id);
    let online_fp_before = fingerprint_of(&hosts_before, online.id);
    assert!(pending_fp_before.is_some(), "seeded pending host must be listed");
    assert!(online_fp_before.is_some(), "seeded online host must be listed");

    let assignable_has = |assignable: &[fleet_db::models::WorkerHost], id: Uuid| {
        assignable
            .iter()
            .any(|h| h.workspace_id == workspace_id && h.id == id)
    };
    let assignable_before =
        worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
            .await
            .expect("list assignable before");
    let pending_assignable_before = assignable_has(&assignable_before, pending.id);
    let online_assignable_before = assignable_has(&assignable_before, online.id);

    workspaces::accept_partners_disclaimer(&pool, workspace_id, Utc::now())
        .await
        .expect("accept disclaimer");

    let hosts_after = worker_hosts::list_for_workspace(&pool, workspace_id)
        .await
        .expect("list hosts after");
    assert_eq!(
        fingerprint_of(&hosts_after, pending.id),
        pending_fp_before,
        "accept must not mutate the seeded pending worker_host"
    );
    assert_eq!(
        fingerprint_of(&hosts_after, online.id),
        online_fp_before,
        "accept must not mutate the seeded online worker_host"
    );

    let assignable_after =
        worker_hosts::list_assignable(&pool, workspace_id, Duration::seconds(30))
            .await
            .expect("list assignable after");
    assert_eq!(
        assignable_has(&assignable_after, pending.id),
        pending_assignable_before,
        "accept must not change workspace-local assignable membership of the seeded pending host"
    );
    assert_eq!(
        assignable_has(&assignable_after, online.id),
        online_assignable_before,
        "accept must not change workspace-local assignable membership of the seeded online host"
    );
}

// ---------------------------------------------------------------------------
// workspace min_isolation: SQL DEFAULT, trim-then-store, missing id
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
    assert_eq!(
        raw, "process",
        "new workspace must default to process (INSERT omits the column)"
    );
}

#[tokio::test]
async fn set_min_isolation_trims_then_stores() {
    let pool = connect_and_migrate().await;
    let (_user_id, workspace_id) = seed_user_with_workspace(&pool).await;

    let stored = workspaces::set_min_isolation(&pool, workspace_id, " vm ")
        .await
        .expect("set padded vm");
    assert_eq!(
        stored, "vm",
        "parse must trim before UPDATE so CHECK accepts"
    );

    let raw: String = sqlx::query_scalar("SELECT min_isolation FROM workspaces WHERE id = $1")
        .bind(workspace_id)
        .fetch_one(&pool)
        .await
        .expect("raw after trim-store");
    assert_eq!(raw, "vm", "column must store the trimmed wire string");
}

#[tokio::test]
async fn min_isolation_missing_workspace_is_not_found() {
    let pool = connect_and_migrate().await;
    let err = workspaces::min_isolation(&pool, Uuid::now_v7())
        .await
        .expect_err("missing workspace");
    assert!(
        matches!(err, DbError::NotFound(_)),
        "min_isolation on unknown id must be NotFound, got {err:?}"
    );
}

#[tokio::test]
async fn set_min_isolation_missing_workspace_is_not_found() {
    let pool = connect_and_migrate().await;
    let err = workspaces::set_min_isolation(&pool, Uuid::now_v7(), "process")
        .await
        .expect_err("missing workspace");
    assert!(
        matches!(err, DbError::NotFound(_)),
        "set_min_isolation on unknown id must be NotFound, got {err:?}"
    );
}

#[tokio::test]
async fn set_min_isolation_process_round_trips() {
    let pool = connect_and_migrate().await;
    let (_user_id, workspace_id) = seed_user_with_workspace(&pool).await;

    workspaces::set_min_isolation(&pool, workspace_id, "container")
        .await
        .expect("seed container");

    let stored = workspaces::set_min_isolation(&pool, workspace_id, "process")
        .await
        .expect("set process");
    assert_eq!(stored, "process");
    assert_eq!(
        workspaces::min_isolation(&pool, workspace_id)
            .await
            .expect("read process"),
        "process"
    );
}
