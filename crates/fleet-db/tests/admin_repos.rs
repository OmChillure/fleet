//! Instance-admin list/count queries.

mod common;

use chrono::Utc;
use common::{connect_and_migrate, unique_email};
use fleet_db::repos::{admin, users, workspaces};
use uuid::Uuid;

#[tokio::test]
async fn list_users_includes_new_signup() {
    let pool = connect_and_migrate().await;
    let email = unique_email();
    let id = Uuid::now_v7();
    let now = Utc::now();
    let mut tx = pool.begin().await.expect("tx");
    users::insert_tx(
        &mut tx,
        &users::NewUser {
            id,
            email: email.clone(),
            email_normalized: email.to_lowercase(),
            display_name: Some("Admin Repo".into()),
            password_hash: "x".into(),
            created_at: now,
        },
    )
    .await
    .expect("insert user");
    workspaces::create_personal_workspace_tx(
        &mut tx,
        Uuid::now_v7(),
        id,
        &format!("ws-{}", &id.to_string()[..8]),
        "Admin Repo's workspace",
        now,
    )
    .await
    .expect("ws");
    tx.commit().await.expect("commit");

    let rows = admin::list_users(&pool).await.expect("list");
    let found = rows
        .iter()
        .find(|u| u.id == id)
        .unwrap_or_else(|| panic!("missing {email}"));
    assert_eq!(found.workspace_count, 1);
    assert!(!found.instance_admin);
    assert_eq!(found.status, "active");

    users::set_instance_admin(&pool, id, true)
        .await
        .expect("set")
        .expect("user");
    assert_eq!(admin::count_users(&pool).await.expect("count") >= 1, true);
    assert!(users::count_instance_admins(&pool).await.expect("admins") >= 1);

    let spaces = admin::list_workspaces(&pool).await.expect("workspaces");
    assert!(spaces.iter().any(|w| w.member_count >= 1));
}
