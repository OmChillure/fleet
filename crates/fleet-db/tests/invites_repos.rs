//! Fail-closed invite repo tests. Expected behavior (expiry, revoke, re-invite).
//! Requires DATABASE_URL — never skips.

mod common;

use chrono::{Duration, Utc};
use fleet_db::WorkspaceInvite;
use fleet_db::repos::users::NewUser;
use fleet_db::repos::{users, workspaces};
use uuid::Uuid;

use common::{connect_and_migrate, unique_email, unique_slug};

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
    workspaces::create_owned(pool, workspace_id, user_id, &slug, "Invite WS", now)
        .await
        .expect("create workspace");
    (user_id, workspace_id)
}

fn new_invite(
    workspace_id: Uuid,
    email: &str,
    invited_by: Uuid,
    expires_at: chrono::DateTime<Utc>,
) -> WorkspaceInvite {
    let now = Utc::now();
    WorkspaceInvite {
        id: Uuid::now_v7(),
        workspace_id,
        email_normalized: email.to_lowercase(),
        role: "member".into(),
        token: format!("tok-{}", Uuid::now_v7()),
        invited_by,
        created_at: now,
        expires_at,
        accepted_at: None,
        accepted_user_id: None,
        revoked_at: None,
    }
}

#[tokio::test]
async fn list_pending_invites_for_email_skips_expired() {
    let pool = connect_and_migrate().await;
    let (owner, ws) = seed_user_with_workspace(&pool).await;
    let email = unique_email();
    let now = Utc::now();
    workspaces::insert_invite(
        &pool,
        &new_invite(ws, &email, owner, now - Duration::hours(1)),
    )
    .await
    .expect("expired invite");

    let pending = workspaces::list_pending_invites_for_email(&pool, &email.to_lowercase(), now)
        .await
        .expect("pending");
    assert!(
        pending.is_empty(),
        "expired invite must not be pending: {pending:?}"
    );

    let live_email = unique_email();
    workspaces::insert_invite(
        &pool,
        &new_invite(ws, &live_email, owner, now + Duration::days(7)),
    )
    .await
    .expect("live invite");
    let live = workspaces::list_pending_invites_for_email(&pool, &live_email.to_lowercase(), now)
        .await
        .expect("live pending");
    assert_eq!(live.len(), 1, "{live:?}");
}

#[tokio::test]
async fn list_open_invites_excludes_expired() {
    let pool = connect_and_migrate().await;
    let (owner, ws) = seed_user_with_workspace(&pool).await;
    let now = Utc::now();
    workspaces::insert_invite(
        &pool,
        &new_invite(ws, &unique_email(), owner, now - Duration::seconds(5)),
    )
    .await
    .expect("expired");
    let live = workspaces::insert_invite(
        &pool,
        &new_invite(ws, &unique_email(), owner, now + Duration::days(1)),
    )
    .await
    .expect("live");

    let open = workspaces::list_open_invites(&pool, ws)
        .await
        .expect("open");
    assert_eq!(
        open.len(),
        1,
        "expired invite must not appear as open: {open:?}"
    );
    assert_eq!(open[0].id, live.id);
}

#[tokio::test]
async fn accept_invite_and_pending_join_refuse_expired() {
    let pool = connect_and_migrate().await;
    let (owner, ws) = seed_user_with_workspace(&pool).await;
    let guest_email = unique_email();
    let now = Utc::now();
    let expired = workspaces::insert_invite(
        &pool,
        &new_invite(ws, &guest_email, owner, now - Duration::minutes(1)),
    )
    .await
    .expect("insert expired");

    let guest = seed_user(&pool, &guest_email).await;
    let accepted = workspaces::accept_invite(&pool, expired.id, guest, now)
        .await
        .expect("accept_invite");
    assert!(
        !accepted,
        "accept_invite must refuse an expired invite"
    );
    assert_eq!(
        workspaces::user_role(&pool, ws, guest).await.expect("role"),
        None,
        "expired accept must not create a membership"
    );

    let n = workspaces::accept_pending_invites_for_user(
        &pool,
        guest,
        &guest_email.to_lowercase(),
        now,
    )
    .await
    .expect("accept pending");
    assert_eq!(n, 0, "expired invite must not join on pending accept");
}

#[tokio::test]
async fn revoke_invite_drops_from_open_and_pending_second_revoke_false() {
    let pool = connect_and_migrate().await;
    let (owner, ws) = seed_user_with_workspace(&pool).await;
    let email = unique_email();
    let now = Utc::now();
    let inv = workspaces::insert_invite(
        &pool,
        &new_invite(ws, &email, owner, now + Duration::days(14)),
    )
    .await
    .expect("insert");

    assert!(
        workspaces::revoke_invite(&pool, ws, inv.id, now)
            .await
            .expect("revoke")
    );
    assert!(
        !workspaces::revoke_invite(&pool, ws, inv.id, now)
            .await
            .expect("second revoke")
    );

    let open = workspaces::list_open_invites(&pool, ws)
        .await
        .expect("open");
    assert!(open.is_empty(), "{open:?}");
    let pending = workspaces::list_pending_invites_for_email(&pool, &email.to_lowercase(), now)
        .await
        .expect("pending");
    assert!(pending.is_empty(), "{pending:?}");
}

#[tokio::test]
async fn expired_invite_does_not_block_reinvite_same_email() {
    let pool = connect_and_migrate().await;
    let (owner, ws) = seed_user_with_workspace(&pool).await;
    let email = unique_email();
    let now = Utc::now();
    workspaces::insert_invite(
        &pool,
        &new_invite(ws, &email, owner, now - Duration::days(1)),
    )
    .await
    .expect("expired");

    let again = workspaces::insert_invite(
        &pool,
        &new_invite(ws, &email, owner, now + Duration::days(14)),
    )
    .await;
    assert!(
        again.is_ok(),
        "expired invite must not block a new invite for the same email: {again:?}"
    );
}
