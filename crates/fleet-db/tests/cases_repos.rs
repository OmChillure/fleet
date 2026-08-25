//! Fail-closed Postgres tests for case reclaim.
//! Requires DATABASE_URL — never skips.

mod common;

use chrono::{Duration, Utc};
use fleet_db::repos::users::NewUser;
use fleet_db::repos::{cases, users, workspaces};
use serde_json::json;
use uuid::Uuid;

use common::{connect_and_migrate, unique_email, unique_slug};

/// Same timeout `fleet-server` `process_due` passes to `reclaim_stale_deciding`.
const STALE_DECIDING_SECS: i64 = 300;

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

async fn seed_user_with_workspace(pool: &fleet_db::PgPool) -> (Uuid, Uuid) {
    let email = unique_email();
    let user_id = seed_user(pool, &email).await;
    let workspace_id = Uuid::now_v7();
    let slug = unique_slug("ws");
    let now = Utc::now();
    workspaces::create_owned(pool, workspace_id, user_id, &slug, "Cases WS", now)
        .await
        .expect("create workspace");
    (user_id, workspace_id)
}

fn status_patch(status: &str, expected_version: i64) -> cases::CasePatch {
    cases::CasePatch {
        status: status.into(),
        stage: "start".into(),
        context: json!({}),
        next_wake_at: None,
        last_error: None,
        run_id: None,
        clear_run_id: false,
        bump_decision: false,
        expected_version,
    }
}

async fn insert_open_case(
    pool: &fleet_db::PgPool,
    workspace_id: Uuid,
    user_id: Uuid,
    title: &str,
) -> fleet_db::Case {
    cases::insert(
        pool,
        &cases::NewCase {
            id: Uuid::now_v7(),
            workspace_id,
            kind: "generic".into(),
            title: title.into(),
            policy_md: String::new(),
            stage: "start".into(),
            context: json!({}),
            next_wake_at: Some(Utc::now() - Duration::seconds(5)),
            created_by: Some(user_id),
        },
    )
    .await
    .expect("insert case")
}

async fn backdate_updated_at(pool: &fleet_db::PgPool, id: Uuid, older_than_secs: i64) {
    sqlx::query("UPDATE cases SET updated_at = now() - make_interval(secs => $2) WHERE id = $1")
        .bind(id)
        .bind(older_than_secs)
        .execute(pool)
        .await
        .expect("backdate updated_at");
}

#[tokio::test]
async fn reclaim_stale_deciding_returns_stuck_deciding_to_waiting() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;

    let stale = insert_open_case(&pool, workspace_id, user_id, "stale deciding").await;
    let stale = cases::apply_patch(
        &pool,
        workspace_id,
        stale.id,
        &status_patch("deciding", stale.version),
    )
    .await
    .expect("patch stale to deciding");
    assert_eq!(stale.decision_count, 0);
    let stale_version = stale.version;
    backdate_updated_at(&pool, stale.id, STALE_DECIDING_SECS + 30).await;

    let fresh = insert_open_case(&pool, workspace_id, user_id, "fresh deciding").await;
    let fresh = cases::apply_patch(
        &pool,
        workspace_id,
        fresh.id,
        &status_patch("deciding", fresh.version),
    )
    .await
    .expect("patch fresh to deciding");
    let fresh_version = fresh.version;

    let n = cases::reclaim_stale_deciding(&pool, STALE_DECIDING_SECS)
        .await
        .expect("reclaim");
    assert!(
        n >= 1,
        "stale deciding case must be reclaimable, rows_affected={n}"
    );

    let stale = cases::get(&pool, workspace_id, stale.id)
        .await
        .expect("get stale")
        .expect("stale row");
    assert_eq!(stale.status, "waiting");
    assert_eq!(stale.decision_count, 0);
    assert_eq!(stale.version, stale_version + 1);
    assert_eq!(
        stale.last_error.as_deref(),
        Some("stale deciding reclaim"),
        "unset last_error is filled on reclaim"
    );
    let wake = stale.next_wake_at.expect("reclaim sets next_wake_at");
    let skew = (Utc::now() - wake).num_seconds().abs();
    assert!(
        skew <= 5,
        "reclaimed next_wake_at must be now, skew={skew}s wake={wake}"
    );

    let fresh = cases::get(&pool, workspace_id, fresh.id)
        .await
        .expect("get fresh")
        .expect("fresh row");
    assert_eq!(
        fresh.status, "deciding",
        "in-flight decide younger than STALE_DECIDING_SECS must stay deciding"
    );
    assert_eq!(
        fresh.version, fresh_version,
        "fresh deciding case must not be version-bumped by reclaim"
    );
}

#[tokio::test]
async fn reclaim_stale_deciding_does_not_revive_terminal_cases() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;

    let mut terminals = Vec::new();
    for status in ["completed", "cancelled", "failed"] {
        let row = insert_open_case(&pool, workspace_id, user_id, status).await;
        let row = cases::apply_patch(
            &pool,
            workspace_id,
            row.id,
            &status_patch(status, row.version),
        )
        .await
        .unwrap_or_else(|e| panic!("patch to {status}: {e}"));
        assert_eq!(row.status, status);
        backdate_updated_at(&pool, row.id, STALE_DECIDING_SECS + 30).await;
        terminals.push((status, row));
    }

    let _ = cases::reclaim_stale_deciding(&pool, STALE_DECIDING_SECS)
        .await
        .expect("reclaim");

    for (status, before) in terminals {
        let after = cases::get(&pool, workspace_id, before.id)
            .await
            .expect("get terminal")
            .expect("terminal row");
        assert_eq!(
            after.status, status,
            "terminal {status} must not be revived into waiting"
        );
        assert_eq!(
            after.version, before.version,
            "terminal {status} must not be updated by reclaim"
        );
    }
}
