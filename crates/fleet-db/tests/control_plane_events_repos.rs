//! Postgres tests for the control-plane event outbox.
//! Requires DATABASE_URL — never skips.

#[allow(dead_code)]
mod common;

use fleet_db::error::DbError;
use fleet_db::repos::control_plane_events;
use serde_json::json;
use uuid::Uuid;

use common::connect_and_migrate;

fn sandbox_failed_body(message_id: Uuid) -> serde_json::Value {
    json!({
        "message_id": message_id,
        "type": "sandbox.failed",
        "ts": "2026-08-24T12:00:00Z",
        "producer": "sandbox-manager",
        "schema_version": "0.1.0",
        "payload": {
            "severity": "failed",
            "reason_code": "oom_killed",
            "message": "Pod OOMKilled by kubelet"
        }
    })
}

/// Drain: insert_event persists unpublished BusMessage rows.
#[tokio::test]
async fn insert_event_starts_unpublished() {
    let pool = connect_and_migrate().await;
    let message_id = Uuid::now_v7();
    let row = control_plane_events::insert_event(
        &pool,
        message_id,
        "sandbox.failed",
        sandbox_failed_body(message_id),
    )
    .await
    .expect("insert");
    assert_eq!(row.message_id, message_id);
    assert!(row.published_at.is_none());
}

/// Drain: list_unpublished is published_at IS NULL, oldest-first.
#[tokio::test]
async fn list_unpublished_returns_inserted_oldest_first() {
    let pool = connect_and_migrate().await;
    let first_id = Uuid::now_v7();
    let second_id = Uuid::now_v7();
    let first = control_plane_events::insert_event(
        &pool,
        first_id,
        "sandbox.failed",
        sandbox_failed_body(first_id),
    )
    .await
    .expect("insert first");
    let second = control_plane_events::insert_event(
        &pool,
        second_id,
        "sandbox.ready",
        json!({ "message_id": second_id, "type": "sandbox.ready" }),
    )
    .await
    .expect("insert second");

    let unpublished = control_plane_events::list_unpublished(&pool, 1000)
        .await
        .expect("list");
    assert!(
        unpublished
            .windows(2)
            .all(|w| { (w[0].created_at, w[0].id) <= (w[1].created_at, w[1].id) }),
        "unpublished page must be oldest-first (created_at, then id)"
    );
    // Shared DB may already have ≥1000 unpublished rows; this test's newest
    // inserts then sit past the drain page. Only assert membership when the
    // page is not already full of older leftovers.
    if unpublished.len() < 1000 {
        let ours: Vec<_> = unpublished
            .iter()
            .filter(|e| e.id == first.id || e.id == second.id)
            .collect();
        assert_eq!(ours.len(), 2, "both inserted events must be unpublished");
        let first_pos = ours
            .iter()
            .position(|e| e.id == first.id)
            .expect("first in drain set");
        let second_pos = ours
            .iter()
            .position(|e| e.id == second.id)
            .expect("second in drain set");
        assert!(first_pos < second_pos, "unpublished must be oldest-first");
    }
}

/// Drain: mark_published stamps published_at once; rematch is a no-op.
#[tokio::test]
async fn mark_published_stamps_once() {
    let pool = connect_and_migrate().await;
    let message_id = Uuid::now_v7();
    let row = control_plane_events::insert_event(
        &pool,
        message_id,
        "sandbox.failed",
        sandbox_failed_body(message_id),
    )
    .await
    .expect("insert");

    assert!(
        control_plane_events::mark_published(&pool, row.id)
            .await
            .expect("mark"),
        "first mark must update the row"
    );
    assert!(
        !control_plane_events::mark_published(&pool, row.id)
            .await
            .expect("mark again"),
        "second mark of the same row is a no-op"
    );

    let still = control_plane_events::list_unpublished(&pool, 1000)
        .await
        .expect("list after mark");
    assert!(
        still.iter().all(|e| e.id != row.id),
        "published row must leave the unpublished set"
    );
}

/// Drain: marking one row leaves a sibling unpublished for retry.
#[tokio::test]
async fn mark_published_leaves_sibling_unpublished() {
    let pool = connect_and_migrate().await;
    let workspace_id = Uuid::now_v7();
    let first_id = Uuid::now_v7();
    let second_id = Uuid::now_v7();
    let first = control_plane_events::insert_event(
        &pool,
        first_id,
        "sandbox.failed",
        scoped_body(first_id, workspace_id),
    )
    .await
    .expect("insert first");
    let second = control_plane_events::insert_event(
        &pool,
        second_id,
        "sandbox.ready",
        scoped_body(second_id, workspace_id),
    )
    .await
    .expect("insert second");

    assert!(
        control_plane_events::mark_published(&pool, first.id)
            .await
            .expect("mark first"),
        "first mark must stamp"
    );

    let first_row = control_plane_events::get_in_workspace(&pool, workspace_id, first.id)
        .await
        .expect("get first")
        .expect("first still in workspace");
    assert!(
        first_row.published_at.is_some(),
        "marked row must be stamped"
    );
    let second_row = control_plane_events::get_in_workspace(&pool, workspace_id, second.id)
        .await
        .expect("get second")
        .expect("sibling still in workspace");
    assert!(
        second_row.published_at.is_none(),
        "second event must remain unpublished"
    );
}

/// subscribe_loop persists inbound samples already published so drain does not re-put them.
#[tokio::test]
async fn insert_inbound_published_absent_from_list_unpublished() {
    let pool = connect_and_migrate().await;
    let message_id = Uuid::now_v7();
    let row = control_plane_events::insert_inbound_published(
        &pool,
        message_id,
        "sandbox.ready",
        json!({ "message_id": message_id, "type": "sandbox.ready" }),
    )
    .await
    .expect("insert inbound published")
    .expect("inbound inserted");

    let unpublished = control_plane_events::list_unpublished(&pool, 1000)
        .await
        .expect("list");
    assert!(
        unpublished.iter().all(|e| e.id != row.id),
        "already-published inbound must not enter the drain set"
    );
}

/// Drain treats mark_published's bool as "this row was stamped".
#[tokio::test]
async fn mark_published_unknown_id_returns_false() {
    let pool = connect_and_migrate().await;
    let workspace_id = Uuid::now_v7();
    let message_id = Uuid::now_v7();
    let sibling = control_plane_events::insert_event(
        &pool,
        message_id,
        "sandbox.failed",
        scoped_body(message_id, workspace_id),
    )
    .await
    .expect("insert sibling");

    assert!(
        !control_plane_events::mark_published(&pool, Uuid::now_v7())
            .await
            .expect("mark unknown"),
        "unknown id must not report stamped"
    );

    let sibling_row = control_plane_events::get_in_workspace(&pool, workspace_id, sibling.id)
        .await
        .expect("get sibling")
        .expect("sibling still in workspace");
    assert!(
        sibling_row.published_at.is_none(),
        "unknown mark must not drain other unpublished rows"
    );
}

/// subscribe_loop inbound persist is ON CONFLICT (message_id) DO NOTHING.
#[tokio::test]
async fn insert_inbound_published_duplicate_message_id_returns_none() {
    let pool = connect_and_migrate().await;
    let message_id = Uuid::now_v7();
    let body = json!({ "message_id": message_id, "type": "sandbox.ready" });
    control_plane_events::insert_inbound_published(
        &pool,
        message_id,
        "sandbox.ready",
        body.clone(),
    )
    .await
    .expect("first inbound")
    .expect("first inbound inserted");

    let second =
        control_plane_events::insert_inbound_published(&pool, message_id, "sandbox.ready", body)
            .await
            .expect("duplicate inbound must not error");
    assert!(
        second.is_none(),
        "duplicate inbound message_id is ON CONFLICT DO NOTHING"
    );
}

#[tokio::test]
async fn duplicate_message_id_is_conflict() {
    let pool = connect_and_migrate().await;
    let message_id = Uuid::now_v7();
    control_plane_events::insert_event(
        &pool,
        message_id,
        "sandbox.failed",
        sandbox_failed_body(message_id),
    )
    .await
    .expect("first insert");

    let err = control_plane_events::insert_event(
        &pool,
        message_id,
        "sandbox.failed",
        sandbox_failed_body(message_id),
    )
    .await
    .expect_err("duplicate message_id must fail");

    match &err {
        DbError::Conflict(msg) => {
            assert_eq!(msg, "unique constraint violated");
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
    assert!(err.is_unique_violation());
}

/// Drain: list_unpublished honors LIMIT so drain_loop cannot load the whole table.
#[tokio::test]
async fn list_unpublished_respects_limit() {
    let pool = connect_and_migrate().await;
    for _ in 0..3 {
        let message_id = Uuid::now_v7();
        control_plane_events::insert_event(
            &pool,
            message_id,
            "sandbox.failed",
            sandbox_failed_body(message_id),
        )
        .await
        .expect("insert");
    }
    let one = control_plane_events::list_unpublished(&pool, 1)
        .await
        .expect("limit 1");
    assert_eq!(
        one.len(),
        1,
        "list_unpublished(1) must page exactly one unpublished row"
    );
    assert!(
        one[0].published_at.is_none(),
        "LIMIT page must be an unpublished row (published_at IS NULL)"
    );
}

/// Drain: list_unpublished(0) clamps to 1 so LIMIT 0 cannot empty the drain.
#[tokio::test]
async fn list_unpublished_zero_limit_clamps_to_one() {
    let pool = connect_and_migrate().await;
    let message_id = Uuid::now_v7();
    control_plane_events::insert_event(
        &pool,
        message_id,
        "sandbox.failed",
        sandbox_failed_body(message_id),
    )
    .await
    .expect("insert");
    let one = control_plane_events::list_unpublished(&pool, 0)
        .await
        .expect("limit 0");
    assert_eq!(
        one.len(),
        1,
        "list_unpublished(0) must clamp to 1, not empty the drain"
    );
}

fn scoped_body(message_id: Uuid, workspace_id: Uuid) -> serde_json::Value {
    json!({
        "message_id": message_id,
        "workspace_id": workspace_id,
        "tenant_id": workspace_id,
        "type": "sandbox.failed",
        "ts": "2026-08-24T12:00:00Z",
        "producer": "sandbox-manager",
        "schema_version": "0.1.0",
        "payload": {
            "severity": "failed",
            "reason_code": "oom_killed",
            "message": "Pod OOMKilled by kubelet"
        }
    })
}

/// Tenant list: list_for_workspace honors LIMIT (same clamp as unpublished drain).
#[tokio::test]
async fn list_for_workspace_respects_limit() {
    let pool = connect_and_migrate().await;
    let workspace_a = Uuid::now_v7();
    for _ in 0..3 {
        let message_id = Uuid::now_v7();
        control_plane_events::insert_event(
            &pool,
            message_id,
            "sandbox.failed",
            scoped_body(message_id, workspace_a),
        )
        .await
        .expect("insert");
    }
    let one = control_plane_events::list_for_workspace(&pool, workspace_a, 1)
        .await
        .expect("limit 1");
    assert_eq!(
        one.len(),
        1,
        "list_for_workspace(A, 1) must page exactly one workspace-A row"
    );
}
