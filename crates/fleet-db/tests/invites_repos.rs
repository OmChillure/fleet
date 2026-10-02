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
async fn revoke_invite_second_revoke_false() {
    let pool = connect_and_migrate().await;
    let (owner, ws) = seed_user_with_workspace(&pool).await;
    let now = Utc::now();
    let inv = workspaces::insert_invite(
        &pool,
        &new_invite(ws, &unique_email(), owner, now + Duration::days(14)),
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

    let got = workspaces::get_invite(&pool, ws, inv.id)
        .await
        .expect("get")
        .expect("revoked row remains");
    assert!(
        got.revoked_at.is_some(),
        "second revoke must leave revoked_at set, not delete the row: {got:?}"
    );
}

#[tokio::test]
async fn revoke_invite_drops_from_list_open() {
    let pool = connect_and_migrate().await;
    let (owner, ws) = seed_user_with_workspace(&pool).await;
    let now = Utc::now();
    let inv = workspaces::insert_invite(
        &pool,
        &new_invite(ws, &unique_email(), owner, now + Duration::days(14)),
    )
    .await
    .expect("insert");

    assert!(
        workspaces::revoke_invite(&pool, ws, inv.id, now)
            .await
            .expect("revoke")
    );

    let open = workspaces::list_open_invites(&pool, ws)
        .await
        .expect("open");
    assert!(
        open.is_empty(),
        "revoked invite must leave list_open: {open:?}"
    );
    let got = workspaces::get_invite(&pool, ws, inv.id)
        .await
        .expect("get")
        .expect("revoked row remains");
    assert!(
        got.revoked_at.is_some(),
        "revoke must SET revoked_at, not delete the row: {got:?}"
    );
}

#[tokio::test]
async fn revoke_invite_wrong_workspace_returns_false_and_row_stays_open() {
    let pool = connect_and_migrate().await;
    let (owner, ws) = seed_user_with_workspace(&pool).await;
    let other_ws = seed_workspace(&pool, owner, "Other WS").await;
    let now = Utc::now();
    let inv = workspaces::insert_invite(
        &pool,
        &new_invite(ws, &unique_email(), owner, now + Duration::days(14)),
    )
    .await
    .expect("insert");

    assert!(
        !workspaces::revoke_invite(&pool, other_ws, inv.id, now)
            .await
            .expect("revoke other workspace")
    );

    let got = workspaces::get_invite(&pool, ws, inv.id)
        .await
        .expect("get")
        .expect("row remains");
    assert!(
        got.revoked_at.is_none(),
        "wrong-workspace revoke must not set revoked_at: {got:?}"
    );
    let open = workspaces::list_open_invites(&pool, ws)
        .await
        .expect("open");
    assert_eq!(
        open.iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![inv.id],
        "invite must stay open after a workspace-mismatched revoke: {open:?}"
    );
}

#[tokio::test]
async fn revoke_invite_already_accepted_returns_false() {
    let pool = connect_and_migrate().await;
    let (owner, ws) = seed_user_with_workspace(&pool).await;
    let guest_email = unique_email();
    let now = Utc::now();
    let inv = workspaces::insert_invite(
        &pool,
        &new_invite(ws, &guest_email, owner, now + Duration::days(14)),
    )
    .await
    .expect("insert");
    let guest = seed_user(&pool, &guest_email).await;
    assert!(
        workspaces::accept_invite(&pool, inv.id, guest, now)
            .await
            .expect("accept")
    );

    assert!(
        !workspaces::revoke_invite(&pool, ws, inv.id, now)
            .await
            .expect("revoke after accept")
    );

    let got = workspaces::get_invite(&pool, ws, inv.id)
        .await
        .expect("get")
        .expect("accepted row remains");
    assert!(
        got.accepted_at.is_some(),
        "revoke after accept must leave accepted_at set: {got:?}"
    );
    assert!(
        got.revoked_at.is_none(),
        "revoke after accept must not set revoked_at: {got:?}"
    );
}

#[tokio::test]
async fn insert_invite_after_revoke_same_email_succeeds() {
    let pool = connect_and_migrate().await;
    let (owner, ws) = seed_user_with_workspace(&pool).await;
    let email = unique_email();
    let now = Utc::now();
    let first = workspaces::insert_invite(
        &pool,
        &new_invite(ws, &email, owner, now + Duration::days(14)),
    )
    .await
    .expect("first");
    assert!(
        workspaces::revoke_invite(&pool, ws, first.id, now)
            .await
            .expect("revoke")
    );

    let again = workspaces::insert_invite(
        &pool,
        &new_invite(ws, &email, owner, now + Duration::days(14)),
    )
    .await
    .expect("re-invite after revoke");

    let open = workspaces::list_open_invites(&pool, ws)
        .await
        .expect("open");
    assert_eq!(
        open.iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![again.id],
        "list_open after revoke+reinvite must be the new id: {open:?}"
    );
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
    workspaces::insert_invite(
        &pool,
        &invite_with_role(ws_other, &other_email, owner_other, later, "admin"),
    )
    .await
    .expect("other email invite");

    let guest = seed_user(&pool, &guest_email).await;
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

    let accepted_a = workspaces::get_invite(&pool, ws_a, inv_a.id)
        .await
        .expect("get A")
        .expect("invite A");
    assert_eq!(accepted_a.accepted_user_id, Some(guest), "{accepted_a:?}");
    let accepted_b = workspaces::get_invite(&pool, ws_b, inv_b.id)
        .await
        .expect("get B")
        .expect("invite B");
    assert_eq!(accepted_b.accepted_user_id, Some(guest), "{accepted_b:?}");
}

/// `add_member` is ON CONFLICT DO NOTHING; a leftover member must not become the invite role.
#[tokio::test]
async fn accept_pending_invites_does_not_promote_existing_member_to_invite_role() {
    let pool = connect_and_migrate().await;
    let (owner_a, ws_a) = seed_user_with_workspace(&pool).await;
    let guest_email = unique_email();
    let now = Utc::now();
    let later = now + Duration::days(3);

    workspaces::insert_invite(
        &pool,
        &invite_with_role(ws_a, &guest_email, owner_a, later, "admin"),
    )
    .await
    .expect("invite A as admin");

    let guest = seed_user(&pool, &guest_email).await;
    workspaces::add_member(&pool, ws_a, guest, "member", now)
        .await
        .expect("pre-existing member on A");

    workspaces::accept_pending_invites_for_user(&pool, guest, &guest_email.to_lowercase(), now)
        .await
        .expect("accept pending");
    assert_eq!(
        workspaces::user_role(&pool, ws_a, guest)
            .await
            .expect("role A"),
        Some("member".into()),
        "accept_pending must not promote an existing member to the invite role"
    );
}

/// Two-row accept siblings stay green under `take(2)`; three live rows must all join.
#[tokio::test]
async fn accept_pending_invites_joins_three_live_workspaces() {
    let pool = connect_and_migrate().await;
    let (owner_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (owner_b, ws_b) = seed_user_with_workspace(&pool).await;
    let (owner_c, ws_c) = seed_user_with_workspace(&pool).await;
    let guest_email = unique_email();
    let now = Utc::now();
    let later = now + Duration::days(14);

    workspaces::insert_invite(
        &pool,
        &invite_with_role(ws_a, &guest_email, owner_a, later, "admin"),
    )
    .await
    .expect("invite A");
    workspaces::insert_invite(
        &pool,
        &invite_with_role(ws_b, &guest_email, owner_b, later, "member"),
    )
    .await
    .expect("invite B");
    workspaces::insert_invite(
        &pool,
        &invite_with_role(ws_c, &guest_email, owner_c, later, "member"),
    )
    .await
    .expect("invite C");

    let guest = seed_user(&pool, &guest_email).await;
    let n =
        workspaces::accept_pending_invites_for_user(&pool, guest, &guest_email.to_lowercase(), now)
            .await
            .expect("accept pending");
    assert_eq!(n, 3, "signup must join every live invite, not stop at two");
    assert_eq!(
        workspaces::user_role(&pool, ws_a, guest)
            .await
            .expect("role A"),
        Some("admin".into())
    );
    assert_eq!(
        workspaces::user_role(&pool, ws_b, guest)
            .await
            .expect("role B"),
        Some("member".into())
    );
    assert_eq!(
        workspaces::user_role(&pool, ws_c, guest)
            .await
            .expect("role C"),
        Some("member".into())
    );
}

/// Already-member continue still `accept_invite`s that row (sibling never reads it).
#[tokio::test]
async fn accept_pending_invites_already_member_still_sets_accepted_at() {
    let pool = connect_and_migrate().await;
    let (owner_a, ws_a) = seed_user_with_workspace(&pool).await;
    let (owner_b, ws_b) = seed_user_with_workspace(&pool).await;
    let guest_email = unique_email();
    let now = Utc::now();
    let later = now + Duration::days(3);

    let inv_a = workspaces::insert_invite(
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

    workspaces::accept_pending_invites_for_user(&pool, guest, &guest_email.to_lowercase(), now)
        .await
        .expect("accept pending");

    let accepted_a = workspaces::get_invite(&pool, ws_a, inv_a.id)
        .await
        .expect("get A")
        .expect("invite A");
    assert_eq!(accepted_a.accepted_user_id, Some(guest), "{accepted_a:?}");
    assert!(
        accepted_a.accepted_at.is_some(),
        "already-member invite must still be accepted, not left pending: {accepted_a:?}"
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

/// `accept_invite` requires `expires_at > now`; an invite whose expiry equals
/// now must stay unbound.
#[tokio::test]
async fn expired_invite_cannot_be_accepted_signup_does_not_join_that_workspace() {
    let pool = connect_and_migrate().await;
    let (owner, ws) = seed_user_with_workspace(&pool).await;
    let guest_email = unique_email();
    let now = Utc::now();

    let inv = workspaces::insert_invite(&pool, &new_invite(ws, &guest_email, owner, now))
        .await
        .expect("expires_at == now");

    let guest = seed_user(&pool, &guest_email).await;
    assert!(
        !workspaces::accept_invite(&pool, inv.id, guest, now)
            .await
            .expect("accept_invite"),
        "accept_invite must refuse an invite whose expires_at equals now"
    );

    let leftover = workspaces::get_invite(&pool, ws, inv.id)
        .await
        .expect("get")
        .expect("row remains");
    assert!(
        leftover.accepted_at.is_none(),
        "expires_at == now must stay unaccepted: {leftover:?}"
    );
    assert!(
        leftover.accepted_user_id.is_none(),
        "expires_at == now must stay unbound: {leftover:?}"
    );
}

/// `list_pending_invites_for_email` is `expires_at > now`; an invite that
/// expires at `now` must not be listed.
#[tokio::test]
async fn list_pending_invites_for_email_skips_expires_at_equal_now() {
    let pool = connect_and_migrate().await;
    let (owner, ws) = seed_user_with_workspace(&pool).await;
    let email = unique_email();
    let now = Utc::now();

    workspaces::insert_invite(&pool, &new_invite(ws, &email, owner, now))
        .await
        .expect("expires_at == now");

    let pending = workspaces::list_pending_invites_for_email(&pool, &email.to_lowercase(), now)
        .await
        .expect("pending");
    assert!(
        pending.is_empty(),
        "expires_at == now must not be pending: {pending:?}"
    );
}

/// Signup walks `list_pending` then `add_member`; an invite that expires at
/// `now` must not join.
#[tokio::test]
async fn accept_pending_invites_for_user_skips_expires_at_equal_now() {
    let pool = connect_and_migrate().await;
    let (owner, ws) = seed_user_with_workspace(&pool).await;
    let guest_email = unique_email();
    let now = Utc::now();

    workspaces::insert_invite(&pool, &new_invite(ws, &guest_email, owner, now))
        .await
        .expect("expires_at == now");

    let guest = seed_user(&pool, &guest_email).await;
    let n =
        workspaces::accept_pending_invites_for_user(&pool, guest, &guest_email.to_lowercase(), now)
            .await
            .expect("accept pending");
    assert_eq!(
        n, 0,
        "signup must skip an invite whose expires_at equals now"
    );
    assert_eq!(
        workspaces::user_role(&pool, ws, guest).await.expect("role"),
        None,
        "expires_at == now must not grant membership"
    );
}
