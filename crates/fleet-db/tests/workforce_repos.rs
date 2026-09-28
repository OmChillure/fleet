//! Fail-closed Postgres tests for workforce goals.
//! Requires DATABASE_URL — never skips.

mod common;

use fleet_db::repos::users::NewUser;
use fleet_db::repos::{cases, users, workforce, workspaces};
use serde_json::json;
use uuid::Uuid;

use common::{connect_and_migrate, unique_email, unique_slug};

async fn seed_user_with_workspace(pool: &fleet_db::PgPool) -> (Uuid, Uuid) {
    let email = unique_email();
    let id = Uuid::now_v7();
    let now = chrono::Utc::now();
    let mut tx = pool.begin().await.expect("begin");
    users::insert_tx(
        &mut tx,
        &NewUser {
            id,
            email: email.clone(),
            email_normalized: email.to_lowercase(),
            display_name: None,
            password_hash: "test-hash".into(),
            created_at: now,
        },
    )
    .await
    .expect("insert user");
    tx.commit().await.expect("commit user");
    let workspace_id = Uuid::now_v7();
    workspaces::create_owned(pool, workspace_id, id, &unique_slug("ws"), "Workforce WS", now)
        .await
        .expect("create workspace");
    (id, workspace_id)
}

async fn insert_workforce_case(
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
            kind: "workforce".into(),
            title: title.into(),
            policy_md: String::new(),
            stage: "start".into(),
            context: json!({}),
            next_wake_at: None,
            created_by: Some(user_id),
        },
    )
    .await
    .expect("insert case")
}

async fn insert_acting_goal(
    pool: &fleet_db::PgPool,
    workspace_id: Uuid,
    user_id: Uuid,
    title: &str,
    run_id: Option<Uuid>,
    case_id: Option<Uuid>,
) -> fleet_db::WorkforceGoal {
    let goal = workforce::insert_goal(
        pool,
        &workforce::NewGoal {
            id: Uuid::now_v7(),
            workspace_id,
            title: title.into(),
            goal_md: title.into(),
            run_id,
            case_id,
            created_by: Some(user_id),
            source_ref: None,
        },
    )
    .await
    .expect("insert goal");
    workforce::set_goal_status(pool, workspace_id, goal.id, "acting", None)
        .await
        .expect("acting")
        .expect("goal remains")
}

#[tokio::test]
async fn pick_goal_for_new_run_does_not_steal() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let run_a = Uuid::new_v4();
    let run_b = Uuid::new_v4();
    let inbox = insert_acting_goal(&pool, workspace_id, user_id, "Inbox", None, None).await;
    let case_a = insert_workforce_case(&pool, workspace_id, user_id, "Owned").await;
    let _owned = insert_acting_goal(
        &pool,
        workspace_id,
        user_id,
        "Owned",
        Some(run_a),
        Some(case_a.id),
    )
    .await;

    let picked = workforce::pick_goal_for_new_run(&pool, workspace_id, run_b, None)
        .await
        .expect("pick")
        .expect("a free goal");
    assert_eq!(
        picked.id, inbox.id,
        "must not steal a new run onto a newer run/case-backed goal"
    );
}

#[tokio::test]
async fn pick_goal_for_new_run_returns_case_owner() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let run_a = Uuid::new_v4();
    let run_b = Uuid::new_v4();
    let case_a = insert_workforce_case(&pool, workspace_id, user_id, "Goal A").await;
    let owner = insert_acting_goal(
        &pool,
        workspace_id,
        user_id,
        "Goal A",
        Some(run_a),
        Some(case_a.id),
    )
    .await;
    let _inbox = insert_acting_goal(&pool, workspace_id, user_id, "Inbox", None, None).await;

    let picked = workforce::pick_goal_for_new_run(&pool, workspace_id, run_b, Some(case_a.id))
        .await
        .expect("pick by case")
        .expect("case owner");
    assert_eq!(picked.id, owner.id);
}

#[tokio::test]
async fn pick_goal_for_new_run_returns_run_owner() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let run_a = Uuid::new_v4();
    let case_a = insert_workforce_case(&pool, workspace_id, user_id, "Goal A").await;
    let owner = insert_acting_goal(
        &pool,
        workspace_id,
        user_id,
        "Goal A",
        Some(run_a),
        Some(case_a.id),
    )
    .await;
    let _inbox = insert_acting_goal(&pool, workspace_id, user_id, "Inbox", None, None).await;

    let picked = workforce::pick_goal_for_new_run(&pool, workspace_id, run_a, None)
        .await
        .expect("pick by run")
        .expect("run owner");
    assert_eq!(picked.id, owner.id);
}

#[tokio::test]
async fn attach_run_does_not_overwrite_existing_run_id() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let run_a = Uuid::new_v4();
    let inbox_run = Uuid::new_v4();
    let case_a = insert_workforce_case(&pool, workspace_id, user_id, "Goal A").await;
    let owner = insert_acting_goal(
        &pool,
        workspace_id,
        user_id,
        "Goal A",
        Some(run_a),
        Some(case_a.id),
    )
    .await;

    let attached = workforce::attach_run(&pool, workspace_id, owner.id, inbox_run)
        .await
        .expect("attach");
    assert!(attached.is_none(), "already-set run_id must not be replaced");
    let owner_now = workforce::get_goal(&pool, workspace_id, owner.id)
        .await
        .expect("reload")
        .expect("owner");
    assert_eq!(owner_now.run_id, Some(run_a), "attach_run must not overwrite");
}

#[tokio::test]
async fn pick_goal_for_new_run_returns_none_when_only_run_or_case_backed_goals() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let run_a = Uuid::new_v4();
    let case_a = insert_workforce_case(&pool, workspace_id, user_id, "Goal A").await;
    let _owned = insert_acting_goal(
        &pool,
        workspace_id,
        user_id,
        "Goal A",
        Some(run_a),
        Some(case_a.id),
    )
    .await;

    let picked = workforce::pick_goal_for_new_run(&pool, workspace_id, Uuid::new_v4(), None)
        .await
        .expect("pick");
    assert!(
        picked.is_none(),
        "inbox pick must stay None when every open goal already has a run or case"
    );
}

#[tokio::test]
async fn pick_goal_for_new_run_unknown_case_does_not_bind_inbox_goal() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let run_a = Uuid::new_v4();
    let case_a = insert_workforce_case(&pool, workspace_id, user_id, "Goal A").await;
    let _owned = insert_acting_goal(
        &pool,
        workspace_id,
        user_id,
        "Goal A",
        Some(run_a),
        Some(case_a.id),
    )
    .await;
    let _inbox = insert_acting_goal(&pool, workspace_id, user_id, "Inbox", None, None).await;
    let other_case_id = Uuid::now_v7();

    let picked =
        workforce::pick_goal_for_new_run(&pool, workspace_id, Uuid::new_v4(), Some(other_case_id))
            .await
            .expect("pick unknown case");
    assert!(
        picked.is_none(),
        "unknown case must not fall through to a runless inbox goal"
    );
}

#[tokio::test]
async fn attach_run_binds_null_run_id() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let inbox_run = Uuid::new_v4();
    let inbox = insert_acting_goal(&pool, workspace_id, user_id, "Inbox", None, None).await;

    let attached = workforce::attach_run(&pool, workspace_id, inbox.id, inbox_run)
        .await
        .expect("attach")
        .expect("null run_id binds");
    assert_eq!(attached.run_id, Some(inbox_run));
    let inbox_now = workforce::get_goal(&pool, workspace_id, inbox.id)
        .await
        .expect("reload")
        .expect("inbox");
    assert_eq!(inbox_now.run_id, Some(inbox_run));
}
