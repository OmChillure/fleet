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
            // Far-future wake so the live host process_due loop cannot
            // claim this row between insert and the test's own patch.
            next_wake_at: Some(Utc::now() + Duration::hours(24)),
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

    let stale = cases::get(&pool, workspace_id, stale.id)
        .await
        .expect("get stale")
        .expect("stale row");
    // n is global (every stale deciding row in the shared DB), so leftover
    // cases from other workspaces can make it > 1. This test locks our row.
    let _ = n;
    if stale.status == "waiting" {
        assert!(
            stale.last_error.is_some(),
            "unset last_error is filled on reclaim"
        );
    } else {
        assert_eq!(
            stale.status, "deciding",
            "live process_due already moved the stale row: {}",
            stale.status
        );
    }
    assert!(
        stale.status == "waiting" || stale.status == "deciding",
        "reclaim leaves waiting, or live process_due re-claims: {}",
        stale.status
    );
    if stale.status == "waiting" {
        let wake = stale.next_wake_at.expect("reclaim sets next_wake_at");
        let skew = (Utc::now() - wake).num_seconds().abs();
        assert!(
            skew <= 5,
            "reclaimed next_wake_at must be now, skew={skew}s wake={wake}"
        );
    }

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
async fn reclaim_stale_deciding_preserves_existing_last_error() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;

    let row = insert_open_case(&pool, workspace_id, user_id, "deciding with last_error").await;
    let prior = "prior last_error from decide".to_string();
    let row = cases::apply_patch(
        &pool,
        workspace_id,
        row.id,
        &cases::CasePatch {
            last_error: Some(prior.clone()),
            ..status_patch("deciding", row.version)
        },
    )
    .await
    .expect("patch deciding with last_error");
    backdate_updated_at(&pool, row.id, STALE_DECIDING_SECS + 30).await;

    let _ = cases::reclaim_stale_deciding(&pool, STALE_DECIDING_SECS)
        .await
        .expect("reclaim");

    let after = cases::get(&pool, workspace_id, row.id)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(after.status, "waiting");
    assert_eq!(
        after.last_error.as_deref(),
        Some(prior.as_str()),
        "reclaim must keep an existing last_error"
    );
}

#[tokio::test]
async fn reclaim_stale_deciding_clamps_stale_after_secs_below_60() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;

    let row = insert_open_case(&pool, workspace_id, user_id, "deciding 30s old").await;
    let row = cases::apply_patch(
        &pool,
        workspace_id,
        row.id,
        &status_patch("deciding", row.version),
    )
    .await
    .expect("patch deciding");
    let version = row.version;
    backdate_updated_at(&pool, row.id, 30).await;

    let _ = cases::reclaim_stale_deciding(&pool, 1)
        .await
        .expect("reclaim with stale_after_secs=1");

    let after = cases::get(&pool, workspace_id, row.id)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(
        after.status, "deciding",
        "stale_after_secs=1 must clamp to 60s; 30s-old deciding stays deciding"
    );
    assert_eq!(
        after.version, version,
        "clamp must leave the 30s-old deciding row untouched"
    );
}

#[tokio::test]
async fn reclaim_stale_deciding_leaves_open_waiting_acting() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let future = Utc::now() + Duration::hours(1);

    let mut rows = Vec::new();
    for status in ["open", "waiting", "acting"] {
        let row = insert_then_patch_due(
            &pool,
            workspace_id,
            user_id,
            status,
            status,
            future,
        )
        .await;
        backdate_updated_at(&pool, row.id, STALE_DECIDING_SECS + 30).await;
        rows.push((status, row));
    }

    let _ = cases::reclaim_stale_deciding(&pool, STALE_DECIDING_SECS)
        .await
        .expect("reclaim");

    for (status, before) in rows {
        let after = cases::get(&pool, workspace_id, before.id)
            .await
            .expect("get")
            .expect("row");
        assert_eq!(
            after.status, status,
            "backdated {status} must keep status after reclaim"
        );
        assert_eq!(
            after.version, before.version,
            "backdated {status} must not be version-bumped by reclaim"
        );
    }
}

/// Production complete/cancel/fail persist `next_wake_at` None. Insert
/// with a future wake first so a parallel `claim_due` cannot snatch the
/// open row, then `status_patch` (wake None). One act: reclaim.
#[tokio::test]
async fn reclaim_stale_deciding_does_not_revive_terminal_cases() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;

    let mut terminals = Vec::new();
    for status in ["completed", "cancelled", "failed"] {
        let row = cases::insert(
            &pool,
            &cases::NewCase {
                id: Uuid::now_v7(),
                workspace_id,
                kind: "generic".into(),
                title: status.into(),
                policy_md: String::new(),
                stage: "start".into(),
                context: json!({}),
                next_wake_at: Some(Utc::now() + Duration::hours(1)),
                created_by: Some(user_id),
            },
        )
        .await
        .unwrap_or_else(|e| panic!("insert {status}: {e}"));
        let row = cases::apply_patch(
            &pool,
            workspace_id,
            row.id,
            &status_patch(status, row.version),
        )
        .await
        .unwrap_or_else(|e| panic!("patch to {status}: {e}"));
        assert_eq!(row.status, status);
        assert!(
            row.next_wake_at.is_none(),
            "complete/cancel/fail persist next_wake_at None"
        );
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
/// Backdated completed/cancelled/failed with a leftover due wake must keep
/// status/version/wake/decision_count. A stale deciding fixture makes
/// reclaim a real UPDATE so this is not claim_due-only.
#[tokio::test]
async fn process_due_reclaim_then_claim_due_does_not_revive_terminals() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let due = Utc::now() - Duration::seconds(15);
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

    let stale = insert_then_patch_due(
        &pool,
        workspace_id,
        user_id,
        &format!("process-due-reclaim-stale-deciding-{run}"),
        "deciding",
        Utc::now() + Duration::hours(1),
    )
    .await;
    backdate_updated_at(&pool, stale.id, STALE_DECIDING_SECS + 30).await;

    let _n = cases::reclaim_stale_deciding(&pool, STALE_DECIDING_SECS)
        .await
        .expect("reclaim");
    let claimed = cases::claim_due(&pool, 50).await.expect("claim_due");

    for (status, before) in terminals {
        assert!(
            !claimed.iter().any(|c| c.id == before.id),
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
}

/// `process_due` reclaims a stale deciding row (→ waiting + due now) then
/// `claim_due` takes it. After both, that id is deciding again.
#[tokio::test]
async fn reclaim_then_claim_due_retries_stale_deciding() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let run = Uuid::now_v7();

    let stale = insert_then_patch_due(
        &pool,
        workspace_id,
        user_id,
        &format!("reclaim-then-claim-stale-deciding-{run}"),
        "deciding",
        Utc::now() + Duration::hours(1),
    )
    .await;
    assert_eq!(stale.status, "deciding");
    let stale_version = stale.version;
    backdate_updated_at(&pool, stale.id, STALE_DECIDING_SECS + 30).await;

    let _n = cases::reclaim_stale_deciding(&pool, STALE_DECIDING_SECS)
        .await
        .expect("reclaim");
    let _ = claim_due_until_live_deciding(&pool, workspace_id, stale.id, &[]).await;

    let after = cases::get(&pool, workspace_id, stale.id)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(
        after.status, "deciding",
        "stale deciding must be deciding again after reclaim+claim_due; status={}",
        after.status
    );
    assert!(
        after.version >= stale_version + 2,
        "reclaim then claim_due must bump version twice (before={stale_version}, after={})",
        after.version
    );
}

/// `apply_decision` wait: status=waiting + bump_decision, matching version.
fn decide_reopen_patch(expected_version: i64) -> cases::CasePatch {
    cases::CasePatch {
        status: "waiting".into(),
        stage: "start".into(),
        context: json!({}),
        next_wake_at: Some(Utc::now()),
        last_error: None,
        run_id: None,
        clear_run_id: false,
        bump_decision: true,
        expected_version,
    }
}

/// Matching-version waiting+bump_decision on completed/cancelled/failed
/// must Conflict and leave status/version/wake/decision_count. apply_patch
/// ANDs status NOT IN those terminals; dropping that predicate would
/// reopen a finished case. Open control still applies so this is not a
/// tautology that every apply_patch is Conflict.
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

        let decide = cases::apply_patch(
            &pool,
            workspace_id,
            row.id,
            &decide_reopen_patch(row.version),
        )
        .await;
        match decide {
            Err(fleet_db::DbError::Conflict(_)) => {}
            other => {
                panic!("waiting+bump_decision on {terminal} must be Conflict, got {other:?}")
            }
        }

        let after = cases::get(&pool, workspace_id, row.id)
            .await
            .expect("get")
            .expect("row");
        assert_row_untouched(&row, &after, &format!("decide on {terminal}"));
    }

    // Control: a non-terminal row can still take the same patch.
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
        &decide_reopen_patch(live.version),
    )
    .await
    .expect("decide on open must still apply");
    assert_eq!(live.status, "waiting");
    assert_eq!(live.decision_count, 1);
}
