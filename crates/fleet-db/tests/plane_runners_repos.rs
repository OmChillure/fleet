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
