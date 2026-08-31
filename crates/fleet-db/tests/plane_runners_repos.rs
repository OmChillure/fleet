//! Postgres tests for the workspace control-plane runner.
//! Requires DATABASE_URL — never skips.

mod common;

use fleet_db::repos::plane_runners::{self, PlaneRunnerPatch};
use fleet_db::repos::workspaces;
use uuid::Uuid;

use common::{connect_and_migrate, unique_slug};

async fn seed_workspace(pool: &fleet_db::PgPool) -> Uuid {
    let user_id = {
        let email = common::unique_email();
        let id = Uuid::now_v7();
        let now = chrono::Utc::now();
        let mut tx = pool.begin().await.expect("begin");
        fleet_db::repos::users::insert_tx(
            &mut tx,
            &fleet_db::repos::users::NewUser {
                id,
                email: email.clone(),
                email_normalized: email.to_lowercase(),
                display_name: None,
                password_hash: "test-hash".into(),
                created_at: now,
            },
        )
        .await
        .expect("user");
        tx.commit().await.expect("commit");
        id
    };
    let workspace_id = Uuid::now_v7();
    workspaces::create_owned(
        pool,
        workspace_id,
        user_id,
        &unique_slug("pl"),
        "Plane WS",
        chrono::Utc::now(),
    )
    .await
    .expect("workspace");
    workspace_id
}

#[tokio::test]
async fn missing_row_defaults_to_taking_native_jobs() {
    let pool = connect_and_migrate().await;
    let ws = seed_workspace(&pool).await;
    assert!(
        plane_runners::get(&pool, ws).await.expect("get").is_none(),
        "missing workspace_plane_runners row must not be invented by get"
    );
    assert!(
        plane_runners::taking_native_jobs(&pool, ws)
            .await
            .expect("taking"),
        "no row must keep today's local spawn"
    );
    let row = plane_runners::get_or_default(&pool, ws)
        .await
        .expect("default");
    assert!(row.taking_native_jobs());
    assert_eq!(row.name, "Control plane");
    assert!(row.allow_host);
    assert!(row.allow_container);
    assert_eq!(row.max_concurrent, 4);
    assert_eq!(row.workspace_id, ws);
}

#[tokio::test]
async fn upsert_persists_stop_taking_jobs() {
    let pool = connect_and_migrate().await;
    let ws = seed_workspace(&pool).await;
    let patched = plane_runners::upsert(
        &pool,
        ws,
        &PlaneRunnerPatch {
            name: Some("This plane".into()),
            allow_host: Some(false),
            allow_container: Some(false),
            max_concurrent: Some(2),
        },
    )
    .await
    .expect("upsert");
    assert_eq!(patched.name, "This plane");
    assert!(!patched.allow_host);
    assert_eq!(patched.max_concurrent, 2);
    assert!(
        !plane_runners::taking_native_jobs(&pool, ws)
            .await
            .expect("taking"),
        "allow_host false must gate local spawn"
    );
    let again = plane_runners::get(&pool, ws)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(again.name, "This plane");
    assert!(!again.allow_host);
    assert!(!again.allow_container);
    assert_eq!(again.max_concurrent, 2);
    assert_eq!(again.workspace_id, ws);
}

/// Second upsert must UPDATE the same workspace row. Unset patch fields
/// keep the persisted values — they must not snap back to missing-row defaults.
#[tokio::test]
async fn second_upsert_updates_without_resetting_unset_fields() {
    let pool = connect_and_migrate().await;
    let ws = seed_workspace(&pool).await;

    plane_runners::upsert(
        &pool,
        ws,
        &PlaneRunnerPatch {
            name: Some("This plane".into()),
            allow_host: Some(false),
            allow_container: Some(false),
            max_concurrent: Some(2),
        },
    )
    .await
    .expect("first upsert");

    let second = plane_runners::upsert(
        &pool,
        ws,
        &PlaneRunnerPatch {
            name: Some("Deck two".into()),
            allow_host: None,
            allow_container: None,
            max_concurrent: None,
        },
    )
    .await
    .expect("second upsert");
    assert_eq!(second.name, "Deck two");
    assert!(
        !second.allow_host,
        "partial second upsert must not restore default allow_host true"
    );
    assert!(!second.allow_container);
    assert_eq!(
        second.max_concurrent, 2,
        "partial second upsert must keep persisted max_concurrent"
    );
    assert!(
        !plane_runners::taking_native_jobs(&pool, ws)
            .await
            .expect("taking"),
        "taking_jobs must stay off after a name-only second upsert"
    );

    let loaded = plane_runners::get(&pool, ws)
        .await
        .expect("get")
        .expect("row");
    assert_eq!(loaded.name, "Deck two");
    assert!(!loaded.allow_host);
    assert_eq!(loaded.max_concurrent, 2);
}

/// Flipping taking_jobs and max_concurrent on a later upsert must persist
/// both, including turning native jobs back on.
#[tokio::test]
async fn second_upsert_persists_taking_jobs_and_max_concurrent() {
    let pool = connect_and_migrate().await;
    let ws = seed_workspace(&pool).await;

    plane_runners::upsert(
        &pool,
        ws,
        &PlaneRunnerPatch {
            name: None,
            allow_host: Some(false),
            allow_container: None,
            max_concurrent: Some(2),
        },
    )
    .await
    .expect("first upsert");
    assert!(
        !plane_runners::taking_native_jobs(&pool, ws)
            .await
            .expect("taking after first")
    );

    let second = plane_runners::upsert(
        &pool,
        ws,
        &PlaneRunnerPatch {
            name: None,
            allow_host: Some(true),
            allow_container: None,
            max_concurrent: Some(16),
        },
    )
    .await
    .expect("second upsert");
    assert!(second.allow_host);
    assert!(
        second.allow_container,
        "unset allow_container stays the missing-row default true"
    );
    assert_eq!(second.max_concurrent, 16);
    assert!(
        plane_runners::taking_native_jobs(&pool, ws)
            .await
            .expect("taking after second"),
        "allow_host true on second upsert must resume native jobs"
    );

    let loaded = plane_runners::get(&pool, ws)
        .await
        .expect("get")
        .expect("row");
    assert!(loaded.allow_host);
    assert_eq!(loaded.max_concurrent, 16);
}

/// An all-None patch after a non-default row must persist those values, not
/// `default_for` (allow_host true / max 4 / name "Control plane").
#[tokio::test]
async fn empty_second_upsert_keeps_persisted_not_missing_row_defaults() {
    let pool = connect_and_migrate().await;
    let ws = seed_workspace(&pool).await;

    plane_runners::upsert(
        &pool,
        ws,
        &PlaneRunnerPatch {
            name: Some("Held".into()),
            allow_host: Some(false),
            allow_container: Some(false),
            max_concurrent: Some(8),
        },
    )
    .await
    .expect("first upsert");

    let again = plane_runners::upsert(&pool, ws, &PlaneRunnerPatch::default())
        .await
        .expect("empty second upsert");
    assert_eq!(again.name, "Held");
    assert!(!again.allow_host);
    assert!(!again.allow_container);
    assert_eq!(again.max_concurrent, 8);
    assert!(
        !plane_runners::taking_native_jobs(&pool, ws)
            .await
            .expect("taking")
    );
}

#[tokio::test]
async fn first_empty_upsert_persists_default_on_row() {
    let pool = connect_and_migrate().await;
    let ws = seed_workspace(&pool).await;
    assert!(
        plane_runners::get(&pool, ws)
            .await
            .expect("get before")
            .is_none(),
        "missing row is not stored until upsert"
    );

    let row = plane_runners::upsert(&pool, ws, &PlaneRunnerPatch::default())
        .await
        .expect("upsert defaults");
    assert_eq!(row.name, "Control plane");
    assert!(row.allow_host);
    assert!(row.allow_container);
    assert_eq!(row.max_concurrent, 4);
    assert!(
        plane_runners::taking_native_jobs(&pool, ws)
            .await
            .expect("taking")
    );

    let stored = plane_runners::get(&pool, ws)
        .await
        .expect("get after")
        .expect("row");
    assert!(stored.allow_host);
    assert_eq!(stored.max_concurrent, 4);
}

#[tokio::test]
async fn upsert_empty_or_blank_name_keeps_current_and_trims() {
    let pool = connect_and_migrate().await;
    let ws = seed_workspace(&pool).await;

    let trimmed = plane_runners::upsert(
        &pool,
        ws,
        &PlaneRunnerPatch {
            name: Some("  Deck  ".into()),
            allow_host: None,
            allow_container: None,
            max_concurrent: None,
        },
    )
    .await
    .expect("trim");
    assert_eq!(trimmed.name, "Deck");

    for blank in ["", "   ", "\t"] {
        let kept = plane_runners::upsert(
            &pool,
            ws,
            &PlaneRunnerPatch {
                name: Some(blank.into()),
                allow_host: None,
                allow_container: None,
                max_concurrent: None,
            },
        )
        .await
        .unwrap_or_else(|_| panic!("blank name {blank:?}"));
        assert_eq!(
            kept.name, "Deck",
            "empty/whitespace name {blank:?} must not overwrite"
        );
    }
}

#[tokio::test]
async fn upsert_clamps_max_concurrent_to_1_256() {
    let pool = connect_and_migrate().await;
    let ws = seed_workspace(&pool).await;

    for (raw, want) in [
        (0, 1),
        (-5, 1),
        (1, 1),
        (256, 256),
        (257, 256),
        (9_999, 256),
    ] {
        let row = plane_runners::upsert(
            &pool,
            ws,
            &PlaneRunnerPatch {
                name: None,
                allow_host: None,
                allow_container: None,
                max_concurrent: Some(raw),
            },
        )
        .await
        .unwrap_or_else(|e| panic!("upsert max_concurrent={raw}: {e}"));
        assert_eq!(row.max_concurrent, want, "raw {raw}");
        let stored = plane_runners::get(&pool, ws)
            .await
            .expect("get")
            .expect("row");
        assert_eq!(stored.max_concurrent, want, "stored raw {raw}");
    }
}

/// Native jobs follow `allow_host` only. `allow_container` is independent.
#[tokio::test]
async fn taking_native_jobs_follows_allow_host_not_container() {
    let pool = connect_and_migrate().await;
    let ws = seed_workspace(&pool).await;

    plane_runners::upsert(
        &pool,
        ws,
        &PlaneRunnerPatch {
            name: None,
            allow_host: Some(true),
            allow_container: Some(false),
            max_concurrent: None,
        },
    )
    .await
    .expect("host on, container off");
    assert!(
        plane_runners::taking_native_jobs(&pool, ws)
            .await
            .expect("taking"),
        "allow_host true must take native jobs even if allow_container is false"
    );

    plane_runners::upsert(
        &pool,
        ws,
        &PlaneRunnerPatch {
            name: None,
            allow_host: Some(false),
            allow_container: Some(true),
            max_concurrent: None,
        },
    )
    .await
    .expect("host off, container on");
    assert!(
        !plane_runners::taking_native_jobs(&pool, ws)
            .await
            .expect("taking"),
        "allow_host false must gate native jobs even if allow_container is true"
    );
}

#[tokio::test]
async fn upsert_does_not_affect_another_workspace() {
    let pool = connect_and_migrate().await;
    let ws_a = seed_workspace(&pool).await;
    let ws_b = seed_workspace(&pool).await;

    plane_runners::upsert(
        &pool,
        ws_a,
        &PlaneRunnerPatch {
            name: Some("A".into()),
            allow_host: Some(false),
            allow_container: Some(false),
            max_concurrent: Some(8),
        },
    )
    .await
    .expect("upsert a");

    assert!(
        plane_runners::taking_native_jobs(&pool, ws_b)
            .await
            .expect("taking b"),
        "missing row on B stays default-on"
    );
    assert!(
        plane_runners::get(&pool, ws_b)
            .await
            .expect("get b")
            .is_none()
    );
    let default_b = plane_runners::get_or_default(&pool, ws_b)
        .await
        .expect("default b");
    assert!(default_b.allow_host);
    assert_eq!(default_b.max_concurrent, 4);
    assert_eq!(default_b.name, "Control plane");

    let a = plane_runners::get(&pool, ws_a)
        .await
        .expect("get a")
        .expect("row a");
    assert!(!a.allow_host);
    assert_eq!(a.max_concurrent, 8);
    assert_eq!(a.name, "A");
}

#[tokio::test]
async fn plane_id_is_stable_and_not_a_workspace_id() {
    let ws = Uuid::now_v7();
    let a = fleet_db::control_plane_runner_id(ws);
    let b = fleet_db::control_plane_runner_id(ws);
    assert_eq!(a, b);
    assert_ne!(a, ws);
    assert!(fleet_db::is_control_plane_runner_id(ws, a));
}
