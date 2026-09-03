//! Fail-closed Postgres tests for case reclaim and process_due claim.
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

fn due_wake_patch(
    status: &str,
    expected_version: i64,
    next_wake_at: chrono::DateTime<Utc>,
) -> cases::CasePatch {
    cases::CasePatch {
        status: status.into(),
        stage: "start".into(),
        context: json!({}),
        next_wake_at: Some(next_wake_at),
        last_error: None,
        run_id: None,
        clear_run_id: false,
        bump_decision: false,
        expected_version,
    }
}

/// Insert open with a future wake so a parallel `claim_due` cannot snatch
/// the row before we patch it to the intended status.
async fn insert_then_patch_due(
    pool: &fleet_db::PgPool,
    workspace_id: Uuid,
    user_id: Uuid,
    title: &str,
    status: &str,
    next_wake_at: chrono::DateTime<Utc>,
) -> fleet_db::Case {
    let row = cases::insert(
        pool,
        &cases::NewCase {
            id: Uuid::now_v7(),
            workspace_id,
            kind: "generic".into(),
            title: title.into(),
            policy_md: String::new(),
            stage: "start".into(),
            context: json!({}),
            next_wake_at: Some(Utc::now() + Duration::hours(1)),
            created_by: Some(user_id),
        },
    )
    .await
    .unwrap_or_else(|e| panic!("insert {title}: {e}"));
    cases::apply_patch(
        pool,
        workspace_id,
        row.id,
        &due_wake_patch(status, row.version, next_wake_at),
    )
    .await
    .unwrap_or_else(|e| panic!("patch {title} to {status}: {e}"))
}

fn assert_row_untouched(before: &fleet_db::Case, after: &fleet_db::Case, label: &str) {
    assert_eq!(
        after.status, before.status,
        "{label} must not change status (was {}, now {})",
        before.status, after.status
    );
    assert_eq!(
        after.version, before.version,
        "{label} must not be version-bumped"
    );
    assert_eq!(
        after.next_wake_at, before.next_wake_at,
        "{label} next_wake_at must be left alone"
    );
    assert_eq!(
        after.decision_count, before.decision_count,
        "{label} decision_count must be left alone"
    );
}

/// `claim_due` is process-global + `SKIP LOCKED`. Drive it until `live_id`
/// is `deciding` on a fresh `get()` — not until this process's batches
/// happen to include that id. Collect every claimed id this process saw
/// so terminals can be asserted absent from the claim set.
async fn claim_due_until_live_deciding(
    pool: &fleet_db::PgPool,
    workspace_id: Uuid,
    live_id: Uuid,
    forbidden: &[(&str, Uuid)],
) -> Vec<Uuid> {
    let mut claimed_ids = Vec::new();
    for _ in 0..16 {
        let live_now = cases::get(pool, workspace_id, live_id)
            .await
            .expect("get live during claim")
            .expect("live row");
        if live_now.status == "deciding" {
            break;
        }
        let batch = cases::claim_due(pool, 50).await.expect("claim_due");
        for claimed in &batch {
            for (label, id) in forbidden {
                assert_ne!(
                    claimed.id, *id,
                    "process_due/claim_due must not claim {label} (id={id})"
                );
            }
            claimed_ids.push(claimed.id);
        }
        if batch.is_empty() {
            tokio::task::yield_now().await;
        }
    }
    claimed_ids
}

/// `process_due` claims via `claim_due`. A leftover due `next_wake_at` on a
/// completed/cancelled/failed row must not flip the row to `deciding`.
///
/// Isolation is by row id + `get()`, not workspace/title. A parallel
/// `process_due` may claim the live waiting row (global `SKIP LOCKED`);
/// that row must still be `deciding` after claim, and terminals must
/// keep status/version/wake/decision_count.
#[tokio::test]
async fn process_due_claim_due_skips_completed_and_cancelled_with_due_wake() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let due = Utc::now() - Duration::seconds(30);
    // Oldest possible wake so other due rows cannot sort ahead of live.
    let live_due = chrono::DateTime::<Utc>::UNIX_EPOCH;
    let run = Uuid::now_v7();

    let mut terminals = Vec::new();
    for status in ["completed", "cancelled", "failed"] {
        let row = insert_then_patch_due(
            &pool,
            workspace_id,
            user_id,
            &format!("process-due-terminal-{status}-{run}"),
            status,
            due,
        )
        .await;
        assert_eq!(row.status, status);
        let wake = row.next_wake_at.expect("terminal keeps due next_wake_at");
        assert!(
            wake <= Utc::now(),
            "{status} next_wake_at must be due, wake={wake}"
        );
        terminals.push((status, row));
    }

    let acting = insert_then_patch_due(
        &pool,
        workspace_id,
        user_id,
        &format!("process-due-acting-{run}"),
        "acting",
        due,
    )
    .await;
    assert_eq!(acting.status, "acting");

    let live = insert_then_patch_due(
        &pool,
        workspace_id,
        user_id,
        &format!("process-due-live-waiting-{run}"),
        "waiting",
        live_due,
    )
    .await;
    assert_eq!(live.status, "waiting");
    let live_version = live.version;

    let mut forbidden: Vec<(&str, Uuid)> = terminals
        .iter()
        .map(|(status, row)| (*status, row.id))
        .collect();
    forbidden.push(("acting", acting.id));

    let claimed_ids = claim_due_until_live_deciding(&pool, workspace_id, live.id, &forbidden).await;

    for (status, before) in &terminals {
        assert!(
            !claimed_ids.contains(&before.id),
            "process_due/claim_due must not return terminal {status} (id={})",
            before.id
        );
        let after = cases::get(&pool, workspace_id, before.id)
            .await
            .expect("get terminal")
            .expect("terminal row");
        assert_row_untouched(before, &after, &format!("terminal {status}"));
        assert_ne!(
            after.status, "deciding",
            "terminal {status} must not re-enter deciding"
        );
    }

    let acting_after = cases::get(&pool, workspace_id, acting.id)
        .await
        .expect("get acting")
        .expect("acting row");
    assert_row_untouched(&acting, &acting_after, "acting");

    // Control: waiting + due is still claimable. `get()`, not membership in
    // this process's batches — a concurrent claimer may have taken the row.
    let live_after = cases::get(&pool, workspace_id, live.id)
        .await
        .expect("get live")
        .expect("live row");
    assert_eq!(
        live_after.status, "deciding",
        "waiting case with due next_wake_at must be deciding after claim_due \
         (this process or a concurrent claimer); status={}",
        live_after.status
    );
    assert!(
        live_after.version >= live_version + 1,
        "claim_due must bump version on the live waiting row (before={live_version}, after={})",
        live_after.version
    );
}

/// Same sequence `process_due` runs: reclaim stale deciding, then claim_due.
/// Terminals stay terminal via `get()` (status/version/wake/decision_count).
/// A live waiting control must be deciding after claim — `get()`, not only
/// membership in this process's batches.
#[tokio::test]
async fn process_due_reclaim_then_claim_due_does_not_revive_terminals() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let due = Utc::now() - Duration::seconds(15);
    let live_due = chrono::DateTime::<Utc>::UNIX_EPOCH;
    let run = Uuid::now_v7();

    let mut terminals = Vec::new();
    for status in ["completed", "cancelled", "failed"] {
        let row = insert_then_patch_due(
            &pool,
            workspace_id,
            user_id,
            &format!("process-due-reclaim-{status}-{run}"),
            status,
            due,
        )
        .await;
        backdate_updated_at(&pool, row.id, STALE_DECIDING_SECS + 30).await;
        terminals.push((status, row));
    }

    let live = insert_then_patch_due(
        &pool,
        workspace_id,
        user_id,
        &format!("process-due-reclaim-live-waiting-{run}"),
        "waiting",
        live_due,
    )
    .await;
    assert_eq!(live.status, "waiting");
    let live_version = live.version;

    let _n = cases::reclaim_stale_deciding(&pool, STALE_DECIDING_SECS)
        .await
        .expect("reclaim");

    let forbidden: Vec<(&str, Uuid)> = terminals
        .iter()
        .map(|(status, row)| (*status, row.id))
        .collect();
    let claimed_ids = claim_due_until_live_deciding(&pool, workspace_id, live.id, &forbidden).await;

    for (status, before) in terminals {
        assert!(
            !claimed_ids.contains(&before.id),
            "process_due must not claim terminal {status} after reclaim"
        );
        let after = cases::get(&pool, workspace_id, before.id)
            .await
            .expect("get")
            .expect("row");
        assert_row_untouched(
            &before,
            &after,
            &format!("terminal {status} after reclaim+claim_due"),
        );
        assert!(
            after.status != "waiting" && after.status != "deciding",
            "reclaim+claim_due must not revive {status} into waiting/deciding"
        );
    }

    let live_after = cases::get(&pool, workspace_id, live.id)
        .await
        .expect("get live")
        .expect("live row");
    assert_eq!(
        live_after.status, "deciding",
        "waiting control must be deciding after reclaim+claim_due; status={}",
        live_after.status
    );
    assert!(
        live_after.version >= live_version + 1,
        "live waiting row must be version-bumped by claim (before={live_version}, after={})",
        live_after.version
    );
}

fn decide_reopen_patch(
    status: &str,
    expected_version: i64,
    bump_decision: bool,
) -> cases::CasePatch {
    cases::CasePatch {
        status: status.into(),
        stage: "start".into(),
        context: json!({}),
        next_wake_at: Some(Utc::now()),
        last_error: None,
        run_id: None,
        clear_run_id: false,
        bump_decision,
        expected_version,
    }
}

/// Manual / HTTP decide on completed/cancelled/failed must fail closed at
/// the row: Conflict, no status change, no decision_count bump, no new
/// decided event. Do not weaken this to Ok(...) to match today's
/// version-only `apply_patch` WHERE.
#[tokio::test]
async fn decide_on_terminal_apply_patch_is_conflict_and_does_not_reopen() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let run = Uuid::now_v7();

    for terminal in ["completed", "cancelled", "failed"] {
        let row = insert_open_case(
            &pool,
            workspace_id,
            user_id,
            &format!("decide-on-terminal-{terminal}-{run}"),
        )
        .await;
        let row = cases::apply_patch(
            &pool,
            workspace_id,
            row.id,
            &status_patch(terminal, row.version),
        )
        .await
        .unwrap_or_else(|e| panic!("seed {terminal}: {e}"));
        assert_eq!(row.status, terminal);
        let before_events = cases::list_events(&pool, row.id, 50)
            .await
            .expect("list events before");
        let before_n = before_events.len();
        let before_kinds: Vec<String> = before_events.iter().map(|e| e.kind.clone()).collect();

        // First step of POST /decide: claim to deciding (no bump).
        let claim = cases::apply_patch(
            &pool,
            workspace_id,
            row.id,
            &decide_reopen_patch("deciding", row.version, false),
        )
        .await;
        match claim {
            Err(fleet_db::DbError::Conflict(_)) => {}
            other => {
                panic!("claim-to-deciding on {terminal} must be Conflict (409), got {other:?}")
            }
        }

        // apply_decision wait: status=waiting + bump_decision.
        let decide = cases::apply_patch(
            &pool,
            workspace_id,
            row.id,
            &decide_reopen_patch("waiting", row.version, true),
        )
        .await;
        match decide {
            Err(fleet_db::DbError::Conflict(_)) => {}
            other => {
                panic!("decide wait patch on {terminal} must be Conflict (409), got {other:?}")
            }
        }

        let after = cases::get(&pool, workspace_id, row.id)
            .await
            .expect("get")
            .expect("row");
        assert_row_untouched(&row, &after, &format!("decide on {terminal}"));
        assert_ne!(
            after.status, "deciding",
            "decide must not reopen {terminal} into deciding"
        );
        assert_ne!(
            after.status, "waiting",
            "decide must not reopen {terminal} into waiting"
        );
        assert_ne!(
            after.status, "open",
            "decide must not reopen {terminal} into open"
        );

        let after_events = cases::list_events(&pool, row.id, 50)
            .await
            .expect("list events after");
        assert_eq!(
            after_events.len(),
            before_n,
            "failed decide on {terminal} must not append an event; before={before_kinds:?} after={:?}",
            after_events
                .iter()
                .map(|e| e.kind.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            !after_events.iter().any(|e| e.kind == "decided"),
            "failed decide on {terminal} must not append kind=decided: {:?}",
            after_events
                .iter()
                .map(|e| e.kind.as_str())
                .collect::<Vec<_>>()
        );
    }

    // Control: a non-terminal row can still take a decide-shaped patch so
    // this is not a tautology that every apply_patch is Conflict.
    let live = insert_open_case(
        &pool,
        workspace_id,
        user_id,
        &format!("decide-on-terminal-live-{run}"),
    )
    .await;
    assert_eq!(live.status, "open");
    let live = cases::apply_patch(
        &pool,
        workspace_id,
        live.id,
        &decide_reopen_patch("waiting", live.version, true),
    )
    .await
    .expect("decide on open must still apply");
    assert_eq!(live.status, "waiting");
    assert_eq!(live.decision_count, 1);
}
