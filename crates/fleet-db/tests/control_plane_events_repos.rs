//! Postgres tests for the control-plane event outbox.
//! Requires DATABASE_URL — never skips.

#[allow(dead_code)]
mod common;

use fleet_db::error::DbError;
use fleet_db::repos::control_plane_events;
use serde_json::json;
use uuid::Uuid;

use common::{connect_and_migrate, require_database_url};

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

#[tokio::test]
async fn insert_list_and_mark_published() {
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
    assert_eq!(first.message_id, first_id);
    assert_eq!(first.event_type, "sandbox.failed");
    assert!(first.published_at.is_none());
    assert_eq!(first.body["type"], "sandbox.failed");

    let second = control_plane_events::insert_event(
        &pool,
        second_id,
        "sandbox.ready",
        json!({ "message_id": second_id, "type": "sandbox.ready" }),
    )
    .await
    .expect("insert second");

    let unpublished = control_plane_events::list_unpublished(&pool, 100)
        .await
        .expect("list");
    assert!(
        unpublished.iter().any(|e| e.id == first.id),
        "first event must be unpublished"
    );
    assert!(
        unpublished.iter().any(|e| e.id == second.id),
        "second event must be unpublished"
    );
    let ours: Vec<_> = unpublished
        .iter()
        .filter(|e| e.id == first.id || e.id == second.id)
        .collect();
    assert_eq!(ours.len(), 2);
    assert!(
        ours.iter().position(|e| e.id == first.id) < ours.iter().position(|e| e.id == second.id),
        "unpublished must be oldest-first"
    );

    assert!(
        control_plane_events::mark_published(&pool, first.id)
            .await
            .expect("mark"),
        "first mark must update the row"
    );
    assert!(
        !control_plane_events::mark_published(&pool, first.id)
            .await
            .expect("mark again"),
        "second mark of the same row is a no-op"
    );

    let still = control_plane_events::list_unpublished(&pool, 1000)
        .await
        .expect("list after mark");
    assert!(
        still.iter().all(|e| e.id != first.id),
        "published row must leave the unpublished set"
    );
    assert!(still.iter().any(|e| e.id == second.id));
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
    assert_eq!(one.len(), 1);
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

/// List/get for workspace A must never return workspace B rows.
#[tokio::test]
async fn list_and_get_never_return_another_workspace_events() {
    let _ = require_database_url();
    let pool = connect_and_migrate().await;

    let workspace_a = Uuid::now_v7();
    let workspace_b = Uuid::now_v7();
    let msg_a = Uuid::now_v7();
    let msg_b_open = Uuid::now_v7();
    let msg_b_published = Uuid::now_v7();
    let msg_a_tenant = Uuid::now_v7();

    let a = control_plane_events::insert_event(
        &pool,
        msg_a,
        "sandbox.failed",
        scoped_body(msg_a, workspace_a),
    )
    .await
    .expect("insert A");

    let a_tenant = control_plane_events::insert_event(
        &pool,
        msg_a_tenant,
        "sandbox.ready",
        json!({
            "message_id": msg_a_tenant,
            "tenant_id": workspace_a,
            "type": "sandbox.ready"
        }),
    )
    .await
    .expect("insert A tenant_id-only");

    let b_open = control_plane_events::insert_event(
        &pool,
        msg_b_open,
        "sandbox.failed",
        scoped_body(msg_b_open, workspace_b),
    )
    .await
    .expect("insert B unpublished");

    let b_pub = control_plane_events::insert_inbound_published(
        &pool,
        msg_b_published,
        "sandbox.ready",
        scoped_body(msg_b_published, workspace_b),
    )
    .await
    .expect("insert B published")
    .expect("B published inserted");

    let listed_a = control_plane_events::list_for_workspace(&pool, workspace_a, 1000)
        .await
        .expect("list A");
    assert!(
        listed_a.iter().any(|e| e.id == a.id),
        "workspace A must see its own event"
    );
    assert!(
        listed_a.iter().any(|e| e.id == a_tenant.id),
        "workspace A must see tenant_id-only rows"
    );
    assert!(
        listed_a
            .iter()
            .all(|e| e.id != b_open.id && e.id != b_pub.id),
        "list for workspace A must not include workspace B rows"
    );

    let listed_b = control_plane_events::list_for_workspace(&pool, workspace_b, 1000)
        .await
        .expect("list B");
    assert!(listed_b.iter().any(|e| e.id == b_open.id));
    assert!(listed_b.iter().any(|e| e.id == b_pub.id));
    assert!(
        listed_b.iter().all(|e| e.id != a.id && e.id != a_tenant.id),
        "list for workspace B must not include workspace A rows"
    );

    let got = control_plane_events::get_in_workspace(&pool, workspace_a, a.id)
        .await
        .expect("get A in A")
        .expect("A must be readable in A");
    assert_eq!(got.id, a.id);
    assert_eq!(got.message_id, msg_a);

    let cross_open = control_plane_events::get_in_workspace(&pool, workspace_a, b_open.id)
        .await
        .expect("cross get unpublished");
    assert!(
        cross_open.is_none(),
        "get in workspace A must not return workspace B unpublished row"
    );

    let cross_pub = control_plane_events::get_in_workspace(&pool, workspace_a, b_pub.id)
        .await
        .expect("cross get published");
    assert!(
        cross_pub.is_none(),
        "get in workspace A must not return workspace B published row"
    );

    let missing = control_plane_events::get_in_workspace(&pool, workspace_a, Uuid::now_v7())
        .await
        .expect("missing id");
    assert!(missing.is_none());
}
