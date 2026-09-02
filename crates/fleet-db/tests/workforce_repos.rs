//! Fail-closed Postgres tests for workforce goals + policy approvals.
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

#[tokio::test]
async fn goal_and_approval_round_trip() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;

    let goal = workforce::insert_goal(
        &pool,
        &workforce::NewGoal {
            id: Uuid::now_v7(),
            workspace_id,
            title: "Ship weather app".into(),
            goal_md: "Done when PR is merged".into(),
            run_id: None,
            case_id: None,
            created_by: Some(user_id),
        },
    )
    .await
    .expect("insert goal");
    assert_eq!(goal.status, "open");

    let approval = workforce::insert_approval(
        &pool,
        &workforce::NewApproval {
            id: Uuid::now_v7(),
            workspace_id,
            goal_id: Some(goal.id),
            task_id: None,
            kind: "merge".into(),
            provider: "github".into(),
            operation: "merge_pr".into(),
            args: json!({ "number": 12 }),
            summary: "Merge PR #12".into(),
        },
    )
    .await
    .expect("insert approval");
    assert_eq!(approval.status, "pending");

    let pending = workforce::list_pending_approvals(&pool, workspace_id)
        .await
        .expect("list pending");
    assert_eq!(pending.len(), 1);

    let decided = workforce::decide_approval(&pool, workspace_id, approval.id, "approved", user_id)
        .await
        .expect("decide")
        .expect("row");
    assert_eq!(decided.status, "approved");
    assert_eq!(decided.decided_by, Some(user_id));

    let again = workforce::decide_approval(&pool, workspace_id, approval.id, "denied", user_id)
        .await
        .expect("second decide");
    assert!(again.is_none(), "cannot decide a non-pending approval");

    let open = workforce::list_open_goals(&pool, workspace_id)
        .await
        .expect("open goals");
    assert_eq!(open.len(), 1);
}

#[tokio::test]
async fn pick_goal_for_new_run_does_not_steal() {
    let pool = connect_and_migrate().await;
    let (user_id, workspace_id) = seed_user_with_workspace(&pool).await;
    let run_a = Uuid::new_v4();
    let run_b = Uuid::new_v4();
    let case_a = cases::insert(
        &pool,
        &cases::NewCase {
            id: Uuid::now_v7(),
            workspace_id,
            kind: "workforce".into(),
            title: "Goal A".into(),
            policy_md: String::new(),
            stage: "start".into(),
            context: json!({}),
            next_wake_at: None,
            created_by: Some(user_id),
        },
    )
    .await
    .expect("case a");
    let first = workforce::insert_goal(
        &pool,
        &workforce::NewGoal {
            id: Uuid::now_v7(),
            workspace_id,
            title: "Goal A".into(),
            goal_md: "A".into(),
            run_id: Some(run_a),
            case_id: Some(case_a.id),
            created_by: Some(user_id),
        },
    )
    .await
    .expect("goal a");
    workforce::set_goal_status(&pool, workspace_id, first.id, "acting", None)
        .await
        .expect("acting a");
    let second = workforce::insert_goal(
        &pool,
        &workforce::NewGoal {
            id: Uuid::now_v7(),
            workspace_id,
            title: "Goal B".into(),
            goal_md: "B".into(),
            run_id: None,
            case_id: None,
            created_by: Some(user_id),
        },
    )
    .await
    .expect("goal b");
    workforce::set_goal_status(&pool, workspace_id, second.id, "acting", None)
        .await
        .expect("acting b");

    let picked = workforce::pick_goal_for_new_run(&pool, workspace_id, run_b, None)
        .await
        .expect("pick")
        .expect("a free goal");
    assert_eq!(picked.id, second.id, "must not attach run B to goal A");

    let by_case = workforce::pick_goal_for_new_run(&pool, workspace_id, run_b, Some(case_a.id))
        .await
        .expect("pick by case")
        .expect("case owner");
    assert_eq!(by_case.id, first.id);

    let by_run = workforce::pick_goal_for_new_run(&pool, workspace_id, run_a, None)
        .await
        .expect("pick by run")
        .expect("run owner");
    assert_eq!(by_run.id, first.id);

    let inbox_run = Uuid::new_v4();
    let inbox_pick = workforce::pick_goal_for_new_run(&pool, workspace_id, inbox_run, None)
        .await
        .expect("inbox pick");
    assert_eq!(
        inbox_pick.as_ref().map(|g| g.id),
        Some(second.id),
        "inbox/chat may bind a runless goalless row, not a case-backed goal"
    );
    workforce::attach_run(&pool, workspace_id, first.id, inbox_run)
        .await
        .expect("cas");
    let first_now = workforce::get_goal(&pool, workspace_id, first.id)
        .await
        .expect("reload")
        .expect("first");
    assert_eq!(first_now.run_id, Some(run_a), "attach_run must not overwrite");
}
