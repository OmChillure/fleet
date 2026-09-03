//! Fail-closed invite repo tests. Expected behavior (expiry, revoke, re-invite).
//! Requires DATABASE_URL — never skips.

mod common;

use chrono::{Duration, Utc};
use fleet_db::WorkspaceInvite;
use fleet_db::error::DbError;
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
    assert!(!accepted, "accept_invite must refuse an expired invite");
    assert_eq!(
        workspaces::user_role(&pool, ws, guest).await.expect("role"),
        None,
        "expired accept must not create a membership"
    );

    let n =
        workspaces::accept_pending_invites_for_user(&pool, guest, &guest_email.to_lowercase(), now)
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
    let expired = workspaces::insert_invite(
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

    let stale = workspaces::get_invite(&pool, ws, expired.id)
        .await
        .expect("get expired")
        .expect("expired row remains");
    assert!(
        stale.revoked_at.is_some(),
        "insert_invite must revoke the expired same-email open row: {stale:?}"
    );
    assert!(
        stale.accepted_at.is_none(),
        "re-invite must not accept the expired row: {stale:?}"
    );
}

#[tokio::test]
async fn insert_invite_unexpired_same_email_is_conflict() {
    let pool = connect_and_migrate().await;
    let (owner, ws) = seed_user_with_workspace(&pool).await;
    let email = unique_email();
    let now = Utc::now();
    let first = workspaces::insert_invite(
        &pool,
        &new_invite(ws, &email, owner, now + Duration::days(14)),
    )
    .await
    .expect("first open invite");

    let err = workspaces::insert_invite(
        &pool,
        &new_invite(ws, &email, owner, now + Duration::days(14)),
    )
    .await
    .expect_err("unexpired duplicate must fail");
    match &err {
        DbError::Conflict(msg) => {
            assert_eq!(msg, "unique constraint violated");
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
    assert!(
        err.is_unique_violation(),
        "unexpired same-email insert must be a unique violation: {err:?}"
    );

    let still = workspaces::get_invite(&pool, ws, first.id)
        .await
        .expect("get first")
        .expect("first row remains");
    assert!(
        still.revoked_at.is_none(),
        "failed duplicate insert must not revoke the live invite: {still:?}"
    );
    assert!(still.accepted_at.is_none(), "{still:?}");
}

async fn seed_workspace(pool: &fleet_db::PgPool, owner: Uuid, name: &str) -> Uuid {
    let workspace_id = Uuid::now_v7();
    let slug = unique_slug("ws");
    let now = Utc::now();
    workspaces::create_owned(pool, workspace_id, owner, &slug, name, now)
        .await
        .expect("create workspace");
    workspace_id
}

fn invite_with_role(
    workspace_id: Uuid,
    email: &str,
    invited_by: Uuid,
    expires_at: chrono::DateTime<Utc>,
    role: &str,
) -> WorkspaceInvite {
    let mut inv = new_invite(workspace_id, email, invited_by, expires_at);
    inv.role = role.into();
    inv
}

/// Signup/login call `accept_pending_invites_for_user`. Two (or more) workspaces
/// can invite the same guest; every unexpired, unrevoked row must join.
#[tokio::test]
async fn accept_pending_invites_joins_every_multiple_pending_workspace() {
    let pool = connect_and_migrate().await;
    let (owner_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (owner_b, ws_b) = seed_user_with_workspace(&pool).await;
    let (owner_expired, ws_expired) = seed_user_with_workspace(&pool).await;
    let (owner_revoked, ws_revoked) = seed_user_with_workspace(&pool).await;
    let (owner_other, ws_other) = seed_user_with_workspace(&pool).await;

    let guest_email = unique_email();
    let other_email = unique_email();
    let now = Utc::now();
    let later = now + Duration::days(14);

    let inv_a = workspaces::insert_invite(
        &pool,
        &invite_with_role(ws_a, &guest_email, owner_a, later, "admin"),
    )
    .await
    .expect("invite A");
    let inv_b = workspaces::insert_invite(
        &pool,
        &invite_with_role(ws_b, &guest_email, owner_b, later, "member"),
    )
    .await
    .expect("invite B");
    let inv_expired = workspaces::insert_invite(
        &pool,
        &invite_with_role(
            ws_expired,
            &guest_email,
            owner_expired,
            now - Duration::hours(1),
            "admin",
        ),
    )
    .await
    .expect("expired invite");
    let inv_revoked = workspaces::insert_invite(
        &pool,
        &invite_with_role(ws_revoked, &guest_email, owner_revoked, later, "member"),
    )
    .await
    .expect("revoked invite");
    assert!(
        workspaces::revoke_invite(&pool, ws_revoked, inv_revoked.id, now)
            .await
            .expect("revoke")
    );
    let inv_other = workspaces::insert_invite(
        &pool,
        &invite_with_role(ws_other, &other_email, owner_other, later, "admin"),
    )
    .await
    .expect("other email invite");

    let pending =
        workspaces::list_pending_invites_for_email(&pool, &guest_email.to_lowercase(), now)
            .await
            .expect("list pending before accept");
    let pending_ids: Vec<Uuid> = pending.iter().map(|i| i.id).collect();
    assert_eq!(
        pending.len(),
        2,
        "only the two live same-email invites must be pending: {pending:?}"
    );
    assert!(pending_ids.contains(&inv_a.id), "{pending:?}");
    assert!(pending_ids.contains(&inv_b.id), "{pending:?}");

    let guest = seed_user(&pool, &guest_email).await;
    let personal = seed_workspace(&pool, guest, "Guest personal").await;

    let n =
        workspaces::accept_pending_invites_for_user(&pool, guest, &guest_email.to_lowercase(), now)
            .await
            .expect("accept pending (signup path)");
    assert_eq!(
        n, 2,
        "signup must join every unexpired invite, not just the first"
    );

    assert_eq!(
        workspaces::user_role(&pool, ws_a, guest)
            .await
            .expect("role A"),
        Some("admin".into()),
        "workspace A must join as invited admin"
    );
    assert_eq!(
        workspaces::user_role(&pool, ws_b, guest)
            .await
            .expect("role B"),
        Some("member".into()),
        "workspace B must join as invited member"
    );
    assert_eq!(
        workspaces::user_role(&pool, personal, guest)
            .await
            .expect("personal"),
        Some("owner".into()),
        "personal workspace from signup must stay owner"
    );
    assert_eq!(
        workspaces::user_role(&pool, ws_expired, guest)
            .await
            .expect("expired role"),
        None,
        "expired invite must not grant membership"
    );
    assert_eq!(
        workspaces::user_role(&pool, ws_revoked, guest)
            .await
            .expect("revoked role"),
        None,
        "revoked invite must not grant membership"
    );
    assert_eq!(
        workspaces::user_role(&pool, ws_other, guest)
            .await
            .expect("other role"),
        None,
        "a different email's invite must not join this user"
    );

    let listed = workspaces::list_for_user(&pool, guest)
        .await
        .expect("list_for_user");
    let listed_ids: Vec<Uuid> = listed.iter().map(|w| w.id).collect();
    assert_eq!(
        listed.len(),
        3,
        "personal + two invited workspaces: {listed:?}"
    );
    assert!(listed_ids.contains(&ws_a), "{listed:?}");
    assert!(listed_ids.contains(&ws_b), "{listed:?}");
    assert!(listed_ids.contains(&personal), "{listed:?}");

    let accepted_a = workspaces::get_invite(&pool, ws_a, inv_a.id)
        .await
        .expect("get A")
        .expect("invite A");
    assert_eq!(accepted_a.accepted_user_id, Some(guest), "{accepted_a:?}");
    assert!(accepted_a.accepted_at.is_some(), "{accepted_a:?}");
    let accepted_b = workspaces::get_invite(&pool, ws_b, inv_b.id)
        .await
        .expect("get B")
        .expect("invite B");
    assert_eq!(accepted_b.accepted_user_id, Some(guest), "{accepted_b:?}");
    assert!(accepted_b.accepted_at.is_some(), "{accepted_b:?}");

    let still_expired = workspaces::get_invite(&pool, ws_expired, inv_expired.id)
        .await
        .expect("get expired")
        .expect("expired row");
    assert!(
        still_expired.accepted_at.is_none(),
        "expired invite must stay unaccepted: {still_expired:?}"
    );
    let still_revoked = workspaces::get_invite(&pool, ws_revoked, inv_revoked.id)
        .await
        .expect("get revoked")
        .expect("revoked row");
    assert!(
        still_revoked.accepted_at.is_none(),
        "revoked invite must stay unaccepted: {still_revoked:?}"
    );
    let still_other = workspaces::get_invite(&pool, ws_other, inv_other.id)
        .await
        .expect("get other")
        .expect("other row");
    assert!(
        still_other.accepted_at.is_none(),
        "other-email invite must stay pending: {still_other:?}"
    );

    let leftover =
        workspaces::list_pending_invites_for_email(&pool, &guest_email.to_lowercase(), now)
            .await
            .expect("pending after");
    assert!(
        leftover.is_empty(),
        "accepted invites must leave the pending list: {leftover:?}"
    );

    let again =
        workspaces::accept_pending_invites_for_user(&pool, guest, &guest_email.to_lowercase(), now)
            .await
            .expect("second accept");
    assert_eq!(
        again, 0,
        "second signup/login must not re-accept consumed invites"
    );
}

/// `list_pending_invites_for_email` is what signup walks; it must not stop at one workspace.
#[tokio::test]
async fn list_pending_invites_for_email_returns_multiple_pending_workspaces() {
    let pool = connect_and_migrate().await;
    let (owner_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (owner_b, ws_b) = seed_user_with_workspace(&pool).await;
    let ws_a2 = seed_workspace(&pool, owner_a, "Owner A second").await;
    let email = unique_email();
    let now = Utc::now();
    let later = now + Duration::days(7);

    let first = {
        let mut inv = new_invite(ws_a, &email, owner_a, later);
        inv.created_at = now - Duration::minutes(2);
        workspaces::insert_invite(&pool, &inv).await.expect("first")
    };
    let second = {
        let mut inv = new_invite(ws_b, &email, owner_b, later);
        inv.created_at = now - Duration::minutes(1);
        workspaces::insert_invite(&pool, &inv)
            .await
            .expect("second")
    };
    let third = {
        let mut inv = invite_with_role(ws_a2, &email, owner_a, later, "admin");
        inv.created_at = now;
        workspaces::insert_invite(&pool, &inv).await.expect("third")
    };

    let pending = workspaces::list_pending_invites_for_email(&pool, &email.to_lowercase(), now)
        .await
        .expect("pending");
    assert_eq!(
        pending.len(),
        3,
        "every unexpired invite for this email must be listed: {pending:?}"
    );
    assert_eq!(
        pending.iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![first.id, second.id, third.id],
        "pending invites must be oldest-first so signup accepts in invite order: {pending:?}"
    );
    let ws_ids: Vec<Uuid> = pending.iter().map(|i| i.workspace_id).collect();
    assert!(ws_ids.contains(&ws_a), "{pending:?}");
    assert!(ws_ids.contains(&ws_b), "{pending:?}");
    assert!(ws_ids.contains(&ws_a2), "{pending:?}");
}

/// Already a member of one invited workspace must not block the rest (signup still joins all).
#[tokio::test]
async fn accept_pending_invites_multiple_pending_continues_if_already_member() {
    let pool = connect_and_migrate().await;
    let (owner_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (owner_b, ws_b) = seed_user_with_workspace(&pool).await;
    let guest_email = unique_email();
    let now = Utc::now();
    let later = now + Duration::days(3);

    workspaces::insert_invite(
        &pool,
        &invite_with_role(ws_a, &guest_email, owner_a, later, "member"),
    )
    .await
    .expect("invite A");
    workspaces::insert_invite(
        &pool,
        &invite_with_role(ws_b, &guest_email, owner_b, later, "admin"),
    )
    .await
    .expect("invite B");

    let guest = seed_user(&pool, &guest_email).await;
    workspaces::add_member(&pool, ws_a, guest, "member", now)
        .await
        .expect("pre-existing membership on A");

    let n =
        workspaces::accept_pending_invites_for_user(&pool, guest, &guest_email.to_lowercase(), now)
            .await
            .expect("accept pending");
    assert_eq!(
        n, 2,
        "already belonging to one invited workspace must not drop the others"
    );
    assert_eq!(
        workspaces::user_role(&pool, ws_a, guest)
            .await
            .expect("role A"),
        Some("member".into())
    );
    assert_eq!(
        workspaces::user_role(&pool, ws_b, guest)
            .await
            .expect("role B"),
        Some("admin".into()),
        "workspace B must still join when A membership already exists"
    );
}

/// Expired invite cannot be accepted; signup (`accept_pending_invites_for_user`)
/// must not join that workspace. An unexpired invite for the same email still
/// applies. The expired row stays expired (unaccepted, not a member).
#[tokio::test]
async fn expired_invite_cannot_be_accepted_signup_does_not_join_that_workspace() {
    let pool = connect_and_migrate().await;
    let (owner_expired, ws_expired) = seed_user_with_workspace(&pool).await;
    let (owner_live, ws_live) = seed_user_with_workspace(&pool).await;
    let guest_email = unique_email();
    let now = Utc::now();

    let expired = workspaces::insert_invite(
        &pool,
        &invite_with_role(
            ws_expired,
            &guest_email,
            owner_expired,
            now - Duration::seconds(1),
            "admin",
        ),
    )
    .await
    .expect("expired invite");
    // Exactly-now is expired too (`expires_at > now` is required to accept).
    let expired_eq = workspaces::insert_invite(
        &pool,
        &invite_with_role(ws_expired, &unique_email(), owner_expired, now, "member"),
    )
    .await
    .expect("expires_at == now");
    let live = workspaces::insert_invite(
        &pool,
        &invite_with_role(
            ws_live,
            &guest_email,
            owner_live,
            now + Duration::days(14),
            "member",
        ),
    )
    .await
    .expect("unexpired invite");

    let pending =
        workspaces::list_pending_invites_for_email(&pool, &guest_email.to_lowercase(), now)
            .await
            .expect("pending before");
    assert_eq!(
        pending.iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![live.id],
        "only the unexpired same-email invite is pending: {pending:?}"
    );

    let guest = seed_user(&pool, &guest_email).await;
    let personal = seed_workspace(&pool, guest, "Guest personal").await;

    let accepted_direct = workspaces::accept_invite(&pool, expired.id, guest, now)
        .await
        .expect("direct accept expired");
    assert!(
        !accepted_direct,
        "accept_invite must refuse an expired invite"
    );
    let accepted_eq = workspaces::accept_invite(&pool, expired_eq.id, guest, now)
        .await
        .expect("direct accept expires_at == now");
    assert!(
        !accepted_eq,
        "accept_invite must refuse an invite whose expires_at equals now"
    );
    assert_eq!(
        workspaces::user_role(&pool, ws_expired, guest)
            .await
            .expect("role after direct refuse"),
        None,
        "refusing accept_invite must not create membership"
    );

    let n =
        workspaces::accept_pending_invites_for_user(&pool, guest, &guest_email.to_lowercase(), now)
            .await
            .expect("signup accept pending");
    assert_eq!(n, 1, "signup must join only the unexpired invite, got {n}");

    assert_eq!(
        workspaces::user_role(&pool, ws_live, guest)
            .await
            .expect("live role"),
        Some("member".into()),
        "unexpired other invite for the same email must still apply"
    );
    assert_eq!(
        workspaces::user_role(&pool, ws_expired, guest)
            .await
            .expect("expired role"),
        None,
        "expired invite must not grant membership on signup"
    );
    assert_eq!(
        workspaces::user_role(&pool, personal, guest)
            .await
            .expect("personal"),
        Some("owner".into()),
        "personal workspace from signup must stay owner"
    );

    let listed = workspaces::list_for_user(&pool, guest)
        .await
        .expect("list_for_user");
    let listed_ids: Vec<Uuid> = listed.iter().map(|w| w.id).collect();
    assert!(listed_ids.contains(&ws_live), "{listed:?}");
    assert!(listed_ids.contains(&personal), "{listed:?}");
    assert!(
        !listed_ids.contains(&ws_expired),
        "signup must not list the expired-invite workspace: {listed:?}"
    );

    let still_expired = workspaces::get_invite(&pool, ws_expired, expired.id)
        .await
        .expect("get expired")
        .expect("expired row remains");
    assert!(
        still_expired.accepted_at.is_none(),
        "expired invite must stay unaccepted: {still_expired:?}"
    );
    assert!(
        still_expired.accepted_user_id.is_none(),
        "expired invite must not bind a user: {still_expired:?}"
    );
    assert!(
        still_expired.revoked_at.is_none(),
        "accept/signup must not rewrite an expired row as revoked: {still_expired:?}"
    );

    let accepted_live = workspaces::get_invite(&pool, ws_live, live.id)
        .await
        .expect("get live")
        .expect("live invite");
    assert_eq!(
        accepted_live.accepted_user_id,
        Some(guest),
        "{accepted_live:?}"
    );
    assert!(accepted_live.accepted_at.is_some(), "{accepted_live:?}");

    let leftover =
        workspaces::list_pending_invites_for_email(&pool, &guest_email.to_lowercase(), now)
            .await
            .expect("pending after");
    assert!(
        leftover.is_empty(),
        "accepted live invite must leave pending: {leftover:?}"
    );

    let again =
        workspaces::accept_pending_invites_for_user(&pool, guest, &guest_email.to_lowercase(), now)
            .await
            .expect("second signup/login");
    assert_eq!(
        again, 0,
        "second accept must not join the expired workspace"
    );
    assert_eq!(
        workspaces::user_role(&pool, ws_expired, guest)
            .await
            .expect("expired role after second accept"),
        None,
        "expired invite must stay not-a-member after a later login"
    );
}
