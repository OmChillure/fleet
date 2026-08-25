//! Fail-closed HTTP integration tests for fleet-auth (in-process Axum oneshot).
//! Requires DATABASE_URL — never skips.

mod common;

use axum::http::StatusCode;
use fleet_auth::{ACCESS_COOKIE, REFRESH_COOKIE};
use serde_json::json;
use uuid::Uuid;

use common::*;

fn workspace_id(login: &HttpOut) -> String {
    login.body["default_workspace"]["id"]
        .as_str()
        .expect("default_workspace.id")
        .to_string()
}

fn user_id(login: &HttpOut) -> String {
    login.body["user"]["id"]
        .as_str()
        .expect("user.id")
        .to_string()
}

fn listed_workspace_ids(listed: &HttpOut) -> Vec<&str> {
    listed.body["workspaces"]
        .as_array()
        .expect("workspaces array")
        .iter()
        .filter_map(|w| w["id"].as_str())
        .collect()
}

#[tokio::test]
async fn signup_201_no_session_cookies() {
    let app = app().await;
    let email = unique_email();
    let out = signup(&app, &email, TEST_PASSWORD).await;

    assert_eq!(out.status, StatusCode::CREATED, "{}", out.body);
    assert_eq!(
        out.body["message"].as_str(),
        Some("account created — please log in"),
        "{}",
        out.body
    );
    assert_eq!(out.body["default_workspace"]["role"], "owner");
    assert!(
        out.cookie(ACCESS_COOKIE).is_none(),
        "signup must not set {ACCESS_COOKIE}: {:?}",
        out.set_cookies()
    );
    assert!(
        out.cookie(REFRESH_COOKIE).is_none(),
        "signup must not set {REFRESH_COOKIE}: {:?}",
        out.set_cookies()
    );
    let me = call(&app, "GET", "/api/auth/me", None, None).await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED, "signup is not a login");
}

#[tokio::test]
async fn signup_rejects_bad_email() {
    let app = app().await;
    let out = signup(&app, "not-an-email", TEST_PASSWORD).await;
    assert_eq!(out.status, StatusCode::BAD_REQUEST, "{}", out.body);
    assert_eq!(out.error(), "valid email is required");
}

#[tokio::test]
async fn signup_rejects_short_password() {
    let app = app().await;
    let out = signup(&app, &unique_email(), "short").await;
    assert_eq!(out.status, StatusCode::BAD_REQUEST, "{}", out.body);
    assert!(
        out.error().contains("8"),
        "short password error should mention 8 characters, got {:?}",
        out.error()
    );

    let exactly_eight = signup(&app, &unique_email(), "12345678").await;
    assert_eq!(
        exactly_eight.status,
        StatusCode::CREATED,
        "password of length 8 must be accepted: {}",
        exactly_eight.body
    );
}

#[tokio::test]
async fn signup_rejects_long_password() {
    let app = app().await;
    let long = "x".repeat(129);
    let out = signup(&app, &unique_email(), &long).await;
    assert_eq!(out.status, StatusCode::BAD_REQUEST, "{}", out.body);
}

#[tokio::test]
async fn signup_duplicate_email_409() {
    let app = app().await;
    let email = unique_email();
    let first = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);

    let second = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(second.status, StatusCode::CONFLICT, "{}", second.body);
    assert_eq!(
        second.error(),
        "an account with this email already exists",
        "{}",
        second.body
    );

    let mixed = format!("Auth.DUP.{}@Example.TEST", Uuid::new_v4());
    let first_mixed = signup(&app, &mixed, TEST_PASSWORD).await;
    assert_eq!(first_mixed.status, StatusCode::CREATED, "{}", first_mixed.body);
    let again_lower = signup(&app, &mixed.to_ascii_lowercase(), TEST_PASSWORD).await;
    assert_eq!(
        again_lower.status,
        StatusCode::CONFLICT,
        "email uniqueness is case-insensitive: {}",
        again_lower.body
    );
}

#[tokio::test]
async fn login_sets_httponly_cookies_and_workspace() {
    let app = app().await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let (out, jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    assert_eq!(out.body["message"].as_str(), Some("logged in"));
    assert_eq!(out.body["default_workspace"]["role"], "owner");

    let access = out
        .cookie(ACCESS_COOKIE)
        .unwrap_or_else(|| panic!("missing {ACCESS_COOKIE} Set-Cookie"));
    let refresh = out
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("missing {REFRESH_COOKIE} Set-Cookie"));
    for c in [&access, &refresh] {
        assert!(c.is_httponly(), "{} should be HttpOnly: {}", c.name, c.raw);
        assert_eq!(c.path(), Some("/"), "{} Path must be /: {}", c.name, c.raw);
        assert!(
            !c.is_secure(),
            "COOKIE_SECURE=false must not set Secure: {}",
            c.raw
        );
    }
    assert!(jar.access().is_some());
    assert!(jar.refresh().is_some());
}

#[tokio::test]
async fn login_unknown_and_bad_password_same_message() {
    let app = app().await;
    let known = unique_email();
    let created = signup(&app, &known, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let unknown = call(
        &app,
        "POST",
        "/api/auth/login",
        None,
        Some(json!({
            "email": unique_email(),
            "password": TEST_PASSWORD,
        })),
    )
    .await;
    let bad = call(
        &app,
        "POST",
        "/api/auth/login",
        None,
        Some(json!({
            "email": known,
            "password": "wrong-password",
        })),
    )
    .await;

    assert_eq!(unknown.status, StatusCode::UNAUTHORIZED, "{}", unknown.body);
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED, "{}", bad.body);
    assert_eq!(unknown.error(), "invalid email or password");
    assert_eq!(bad.error(), unknown.error());
}

#[tokio::test]
async fn concurrent_logins_two_sessions() {
    let app = app().await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let (a_out, a) = login(&app, &email, TEST_PASSWORD).await;
    let (b_out, b) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(a_out.status, StatusCode::OK, "{}", a_out.body);
    assert_eq!(b_out.status, StatusCode::OK, "{}", b_out.body);

    let ra = a.refresh().expect("session A refresh");
    let rb = b.refresh().expect("session B refresh");
    assert_ne!(
        ra, rb,
        "concurrent logins must issue distinct refresh cookies"
    );

    let me_a = call(&app, "GET", "/api/auth/me", Some(&a), None).await;
    let me_b = call(&app, "GET", "/api/auth/me", Some(&b), None).await;
    assert_eq!(me_a.status, StatusCode::OK, "{}", me_a.body);
    assert_eq!(me_b.status, StatusCode::OK, "{}", me_b.body);
}

#[tokio::test]
async fn refresh_rotates_cookies() {
    let app = app().await;
    let (_email, jar, _) = signup_and_login(&app).await;
    let old_access = jar.access().expect("access").to_string();
    let old_refresh = jar.refresh().expect("refresh").to_string();

    let out = call(&app, "POST", "/api/auth/refresh", Some(&jar), None).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);

    let new_access = out
        .cookie(ACCESS_COOKIE)
        .unwrap_or_else(|| panic!("refresh must set {ACCESS_COOKIE}"));
    let new_refresh = out
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("refresh must set {REFRESH_COOKIE}"));
    assert_ne!(new_access.value, old_access);
    assert_ne!(new_refresh.value, old_refresh);
}

#[tokio::test]
async fn refresh_reuse_revokes_family() {
    let app = app().await;
    let (_email, jar, _) = signup_and_login(&app).await;
    let old_refresh = jar.refresh().expect("refresh").to_string();

    let rotated = call(&app, "POST", "/api/auth/refresh", Some(&jar), None).await;
    assert_eq!(rotated.status, StatusCode::OK, "{}", rotated.body);
    let mut rotated_jar = jar.clone();
    rotated.absorb_into(&mut rotated_jar);
    assert_ne!(
        rotated_jar.refresh(),
        Some(old_refresh.as_str()),
        "refresh should rotate the refresh cookie"
    );

    let mut replay = CookieJar::default();
    replay.insert(REFRESH_COOKIE, old_refresh);

    let reuse = call(&app, "POST", "/api/auth/refresh", Some(&replay), None).await;
    assert_eq!(reuse.status, StatusCode::UNAUTHORIZED, "{}", reuse.body);
    let msg = reuse.error().to_ascii_lowercase();
    assert!(
        msg.contains("reuse") || msg.contains("family"),
        "expected reuse/family error, got {:?}",
        reuse.error()
    );

    let me = call(&app, "GET", "/api/auth/me", Some(&rotated_jar), None).await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED, "{}", me.body);

    let again = call(&app, "POST", "/api/auth/refresh", Some(&rotated_jar), None).await;
    assert_eq!(again.status, StatusCode::UNAUTHORIZED, "{}", again.body);
}

#[tokio::test]
async fn logout_then_me_401() {
    let app = app().await;
    let (_email, jar, _) = signup_and_login(&app).await;

    let out = call(&app, "POST", "/api/auth/logout", Some(&jar), None).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);

    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED, "{}", me.body);
}

#[tokio::test]
async fn me_lists_at_least_one_workspace() {
    let app = app().await;
    let (_email, jar, _) = signup_and_login(&app).await;

    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.body);
    let workspaces = me.body["workspaces"].as_array().expect("workspaces array");
    assert!(
        !workspaces.is_empty(),
        "expected at least one workspace: {}",
        me.body
    );
}

#[tokio::test]
async fn create_workspace_slug_and_owner() {
    let app = app().await;
    let (_email, jar, _) = signup_and_login(&app).await;

    let out = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "name": "Acme Eng", "slug": "acme-eng" })),
    )
    .await;
    assert_eq!(out.status, StatusCode::CREATED, "{}", out.body);
    assert_eq!(out.body["workspace"]["role"], "owner");
    assert_eq!(out.body["workspace"]["name"], "Acme Eng");
    let slug = out.body["workspace"]["slug"]
        .as_str()
        .expect("workspace.slug");
    assert!(
        slug.starts_with("acme-eng-"),
        "expected slug derived from request, got {slug:?}"
    );
}

#[tokio::test]
async fn create_workspace_empty_and_too_long_400() {
    let app = app().await;
    let (_email, jar, _) = signup_and_login(&app).await;

    let empty = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "name": "   " })),
    )
    .await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST, "{}", empty.body);

    let too_long = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "name": "n".repeat(81) })),
    )
    .await;
    assert_eq!(
        too_long.status,
        StatusCode::BAD_REQUEST,
        "{}",
        too_long.body
    );
}

#[tokio::test]
async fn other_user_forbidden_on_personas() {
    let app = app().await;
    let (_a_email, _a_jar, a_login) = signup_and_login(&app).await;
    let ws_id = a_login.body["default_workspace"]["id"]
        .as_str()
        .expect("A default_workspace.id")
        .to_string();

    let (_b_email, b_jar, _) = signup_and_login(&app).await;
    let uri = format!("/api/workspaces/{ws_id}/personas");
    let out = call(&app, "GET", &uri, Some(&b_jar), None).await;
    assert_eq!(out.status, StatusCode::FORBIDDEN, "{}", out.body);
    assert!(
        out.error().contains("not a member"),
        "forbidden should say not a member, got {:?}",
        out.error()
    );
}

#[tokio::test]
async fn unauthenticated_cookie_routes_are_401() {
    let app = app().await;
    for (method, uri) in [
        ("GET", "/api/auth/me"),
        ("GET", "/api/workspaces"),
        ("POST", "/api/workspaces"),
    ] {
        let out = call(&app, method, uri, None, Some(json!({ "name": "x" }))).await;
        assert_eq!(
            out.status,
            StatusCode::UNAUTHORIZED,
            "{method} {uri}: {}",
            out.body
        );
    }
}

#[tokio::test]
async fn refresh_without_cookie_is_401() {
    let app = app().await;
    let out = call(&app, "POST", "/api/auth/refresh", None, None).await;
    assert_eq!(out.status, StatusCode::UNAUTHORIZED, "{}", out.body);
    assert_eq!(out.error(), "missing refresh token");
}

#[tokio::test]
async fn logout_clears_cookies_and_blocks_refresh() {
    let app = app().await;
    let (_email, jar, _) = signup_and_login(&app).await;

    let out = call(&app, "POST", "/api/auth/logout", Some(&jar), None).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    assert_eq!(out.body["message"].as_str(), Some("logged out"));

    let access = out
        .cookie(ACCESS_COOKIE)
        .expect("logout must send access Set-Cookie");
    let refresh = out
        .cookie(REFRESH_COOKIE)
        .expect("logout must send refresh Set-Cookie");
    assert_eq!(
        access.max_age(),
        Some(0),
        "logout must expire access cookie: {}",
        access.raw
    );
    assert_eq!(
        refresh.max_age(),
        Some(0),
        "logout must expire refresh cookie: {}",
        refresh.raw
    );

    let refresh_after = call(&app, "POST", "/api/auth/refresh", Some(&jar), None).await;
    assert_eq!(
        refresh_after.status,
        StatusCode::UNAUTHORIZED,
        "revoked refresh must not rotate: {}",
        refresh_after.body
    );
}

#[tokio::test]
async fn login_is_case_insensitive_on_email() {
    let app = app().await;
    let local = format!("Case.{}", Uuid::new_v4());
    let email = format!("{local}@Example.TEST");
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let (out, jar) = login(&app, &email.to_ascii_lowercase(), TEST_PASSWORD).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    assert!(jar.access().is_some());
}

#[tokio::test]
async fn signup_display_name_sets_workspace_name() {
    let app = app().await;
    let email = unique_email();
    let out = signup_with(&app, &email, TEST_PASSWORD, Some("Ada Lovelace")).await;
    assert_eq!(out.status, StatusCode::CREATED, "{}", out.body);
    assert_eq!(
        out.body["default_workspace"]["name"].as_str(),
        Some("Ada Lovelace's workspace"),
        "{}",
        out.body
    );
}

#[tokio::test]
async fn inactive_user_cannot_login() {
    let app = app().await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let pool = connect_and_migrate().await;
    let n = sqlx::query("UPDATE users SET status = 'disabled' WHERE email_normalized = $1")
        .bind(email.to_ascii_lowercase())
        .execute(&pool)
        .await
        .expect("deactivate user");
    assert_eq!(n.rows_affected(), 1, "expected one user row updated");

    let (out, _) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(out.status, StatusCode::UNAUTHORIZED, "{}", out.body);
    assert_eq!(out.error(), "account is not active");
}

#[tokio::test]
async fn create_workspace_then_list_includes_both() {
    let app = app().await;
    let (_email, jar, login_out) = signup_and_login(&app).await;
    let first_id = login_out.body["default_workspace"]["id"]
        .as_str()
        .expect("default workspace")
        .to_string();

    let created = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "name": "Second" })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let second_id = created.body["workspace"]["id"].as_str().unwrap();

    let listed = call(&app, "GET", "/api/workspaces", Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids: Vec<&str> = listed.body["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w["id"].as_str())
        .collect();
    assert!(ids.contains(&first_id.as_str()), "{ids:?}");
    assert!(ids.contains(&second_id), "{ids:?}");
}

#[tokio::test]
async fn invite_adds_existing_user_and_member_cannot_invite() {
    let app = app().await;
    let (owner_email, owner_jar, owner_me) = signup_and_login(&app).await;
    let ws = owner_me.body["default_workspace"]["id"]
        .as_str()
        .or_else(|| {
            owner_me.body["workspaces"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|w| w["id"].as_str())
        })
        .expect("workspace")
        .to_string();
    let _ = owner_email;

    let (member_email, member_jar, _) = signup_and_login(&app).await;

    let invited = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": member_email, "role": "member" })),
    )
    .await;
    assert_eq!(invited.status, StatusCode::CREATED, "{}", invited.body);
    assert_eq!(invited.body["added_existing_user"], true);

    let members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    let roles: Vec<&str> = members.body["members"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["role"].as_str())
        .collect();
    assert!(roles.contains(&"owner"), "{roles:?}");
    assert!(roles.contains(&"member"), "{roles:?}");

    let forbidden = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&member_jar),
        Some(json!({ "email": unique_email(), "role": "member" })),
    )
    .await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN, "{}", forbidden.body);
}

#[tokio::test]
async fn pending_invite_joins_on_signup() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_me) = signup_and_login(&app).await;
    let ws = owner_me.body["default_workspace"]["id"]
        .as_str()
        .or_else(|| {
            owner_me.body["workspaces"]
                .as_array()
                .and_then(|a| a.first())
                .and_then(|w| w["id"].as_str())
        })
        .expect("workspace")
        .to_string();

    let guest = unique_email();
    let inv = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": guest, "role": "admin" })),
    )
    .await;
    assert_eq!(inv.status, StatusCode::CREATED, "{}", inv.body);
    assert_eq!(inv.body["added_existing_user"], false);

    let _ = signup(&app, &guest, TEST_PASSWORD).await;
    let (login_out, guest_jar) = login(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(login_out.status, StatusCode::OK, "{}", login_out.body);

    let listed = call(&app, "GET", "/api/workspaces", Some(&guest_jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids: Vec<&str> = listed.body["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w["id"].as_str())
        .collect();
    assert!(ids.contains(&ws.as_str()), "guest should join invited workspace: {ids:?}");
}

#[tokio::test]
async fn login_accepts_pending_invites() {
    let app = app().await;
    let (owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
    let guest = unique_email();

    let inv = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": guest, "role": "admin" })),
    )
    .await;
    assert_eq!(inv.status, StatusCode::CREATED, "{}", inv.body);
    assert_eq!(inv.body["added_existing_user"], false);

    // Existing user for that email without signup (signup would consume the invite).
    let pool = connect_and_migrate().await;
    let guest_id = Uuid::now_v7();
    let n = sqlx::query(
        r#"
        INSERT INTO users (
            id, email, email_normalized, password_hash, status, created_at, updated_at
        )
        SELECT $1, $2, $3, password_hash, 'active', now(), now()
        FROM users
        WHERE email_normalized = $4
        "#,
    )
    .bind(guest_id)
    .bind(&guest)
    .bind(guest.to_ascii_lowercase())
    .bind(owner_email.to_ascii_lowercase())
    .execute(&pool)
    .await
    .expect("insert guest user");
    assert_eq!(n.rows_affected(), 1, "expected guest user inserted");

    let before = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.body);
    let before_emails: Vec<&str> = before.body["members"]
        .as_array()
        .expect("members")
        .iter()
        .filter_map(|m| m["email"].as_str())
        .collect();
    assert!(
        !before_emails
            .iter()
            .any(|e| e.eq_ignore_ascii_case(&guest)),
        "pending invite must not add the user before login: {before_emails:?}"
    );

    let (login_out, guest_jar) = login(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(login_out.status, StatusCode::OK, "{}", login_out.body);

    let listed = call(&app, "GET", "/api/workspaces", Some(&guest_jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids = listed_workspace_ids(&listed);
    assert!(
        ids.contains(&ws.as_str()),
        "login must accept pending invite and join workspace: {ids:?}"
    );

    let members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&guest_jar),
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    assert_eq!(
        members.body["you"]["role"].as_str(),
        Some("admin"),
        "guest should join as invited role: {}",
        members.body
    );
}

#[tokio::test]
async fn expired_invite_does_not_join_on_signup() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
    let guest = unique_email();

    let inv = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": guest, "role": "member" })),
    )
    .await;
    assert_eq!(inv.status, StatusCode::CREATED, "{}", inv.body);
    assert_eq!(inv.body["added_existing_user"], false);
    let invite_id = inv.body["invite"]["id"]
        .as_str()
        .expect("invite.id")
        .to_string();

    let pool = connect_and_migrate().await;
    let n = sqlx::query(
        "UPDATE workspace_invites SET expires_at = now() - interval '1 minute' WHERE id = $1",
    )
    .bind(Uuid::parse_str(&invite_id).expect("invite uuid"))
    .execute(&pool)
    .await
    .expect("age expires_at");
    assert_eq!(n.rows_affected(), 1, "expected one invite aged");

    let created = signup(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let (login_out, guest_jar) = login(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(login_out.status, StatusCode::OK, "{}", login_out.body);

    let listed = call(&app, "GET", "/api/workspaces", Some(&guest_jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids = listed_workspace_ids(&listed);
    assert!(
        !ids.contains(&ws.as_str()),
        "expired invite must not grant the workspace on signup: {ids:?} contains {ws}"
    );
}

#[tokio::test]
async fn last_owner_cannot_patch_self_to_member() {
    let app = app().await;
    let (_email, jar, login_out) = signup_and_login(&app).await;
    let ws = workspace_id(&login_out);
    let owner_id = user_id(&login_out);

    let out = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{owner_id}"),
        Some(&jar),
        Some(json!({ "role": "member" })),
    )
    .await;
    assert_eq!(out.status, StatusCode::BAD_REQUEST, "{}", out.body);
    let msg = out.error().to_ascii_lowercase();
    assert!(
        msg.contains("last owner"),
        "expected last-owner message, got {:?}",
        out.error()
    );

    let members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    assert_eq!(
        members.body["you"]["role"].as_str(),
        Some("owner"),
        "last owner must still be owner after rejected demotion: {}",
        members.body
    );
}

#[tokio::test]
async fn last_owner_cannot_delete_self() {
    let app = app().await;
    let (_email, jar, login_out) = signup_and_login(&app).await;
    let ws = workspace_id(&login_out);
    let owner_id = user_id(&login_out);

    let out = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/members/{owner_id}"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(out.status, StatusCode::BAD_REQUEST, "{}", out.body);
    let msg = out.error().to_ascii_lowercase();
    assert!(
        msg.contains("last owner"),
        "expected last-owner message, got {:?}",
        out.error()
    );

    let members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    assert_eq!(
        members.body["you"]["role"].as_str(),
        Some("owner"),
        "last owner must still be a member after rejected leave: {}",
        members.body
    );
}

#[tokio::test]
async fn member_cannot_list_or_revoke_invites() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);

    let pending = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": unique_email(), "role": "member" })),
    )
    .await;
    assert_eq!(pending.status, StatusCode::CREATED, "{}", pending.body);
    let invite_id = pending.body["invite"]["id"]
        .as_str()
        .expect("invite.id")
        .to_string();

    let (member_email, member_jar, _) = signup_and_login(&app).await;
    let added = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": member_email, "role": "member" })),
    )
    .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);
    assert_eq!(added.body["added_existing_user"], true);

    let list = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&member_jar),
        None,
    )
    .await;
    assert_eq!(list.status, StatusCode::FORBIDDEN, "{}", list.body);

    let revoke = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/invites/{invite_id}"),
        Some(&member_jar),
        None,
    )
    .await;
    assert_eq!(revoke.status, StatusCode::FORBIDDEN, "{}", revoke.body);

    let still = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(still.status, StatusCode::OK, "{}", still.body);
    let ids: Vec<&str> = still.body["invites"]
        .as_array()
        .expect("invites")
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    assert!(
        ids.contains(&invite_id.as_str()),
        "member revoke must not remove the invite: {ids:?}"
    );
}

#[tokio::test]
async fn revoke_invite_then_signup_does_not_join() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
    let guest = unique_email();

    let inv = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": guest, "role": "member" })),
    )
    .await;
    assert_eq!(inv.status, StatusCode::CREATED, "{}", inv.body);
    assert_eq!(inv.body["added_existing_user"], false);
    let invite_id = inv.body["invite"]["id"]
        .as_str()
        .expect("invite.id")
        .to_string();

    let revoked = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/invites/{invite_id}"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.body);
    assert_eq!(revoked.body["revoked"], true);

    let created = signup(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let (login_out, guest_jar) = login(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(login_out.status, StatusCode::OK, "{}", login_out.body);

    let listed = call(&app, "GET", "/api/workspaces", Some(&guest_jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids = listed_workspace_ids(&listed);
    assert!(
        !ids.contains(&ws.as_str()),
        "revoked invite must not grant the workspace: {ids:?} contains {ws}"
    );
}

#[tokio::test]
async fn invite_existing_member_is_already_a_member() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
    let (member_email, _member_jar, _) = signup_and_login(&app).await;

    let first = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": member_email, "role": "member" })),
    )
    .await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);
    assert_eq!(first.body["added_existing_user"], true);

    let again = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": member_email, "role": "member" })),
    )
    .await;
    assert_eq!(again.status, StatusCode::CONFLICT, "{}", again.body);
    let msg = again.error().to_ascii_lowercase();
    assert!(
        msg.contains("already a member"),
        "expected already-a-member conflict, got {:?}",
        again.error()
    );
}

#[tokio::test]
async fn invite_role_owner_is_rejected() {
    let app = app().await;
    let (_email, jar, login_out) = signup_and_login(&app).await;
    let ws = workspace_id(&login_out);

    let out = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&jar),
        Some(json!({ "email": unique_email(), "role": "owner" })),
    )
    .await;
    assert_eq!(out.status, StatusCode::BAD_REQUEST, "{}", out.body);
    let msg = out.error().to_ascii_lowercase();
    assert!(
        msg.contains("admin") && msg.contains("member"),
        "expected invite role must be admin or member, got {:?}",
        out.error()
    );
}

#[tokio::test]
async fn owner_get_invites_omits_token() {
    let app = app().await;
    let (_email, jar, login_out) = signup_and_login(&app).await;
    let ws = workspace_id(&login_out);

    let inv = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&jar),
        Some(json!({ "email": unique_email(), "role": "member" })),
    )
    .await;
    assert_eq!(inv.status, StatusCode::CREATED, "{}", inv.body);

    let listed = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let invites = listed.body["invites"].as_array().expect("invites array");
    assert!(
        !invites.is_empty(),
        "need at least one invite to check token leakage: {}",
        listed.body
    );
    for invite in invites {
        let obj = invite
            .as_object()
            .unwrap_or_else(|| panic!("invite object: {invite}"));
        assert!(
            !obj.contains_key("token"),
            "invite must not include token: {invite}"
        );
        assert!(
            !obj.contains_key("invite_token"),
            "invite must not include invite_token: {invite}"
        );
    }
}

#[tokio::test]
async fn admin_cannot_invite_admin_can_invite_member() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
    let (member_email, admin_jar, member_login) = signup_and_login(&app).await;
    let member_id = user_id(&member_login);

    let added = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": member_email, "role": "member" })),
    )
    .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);

    let promoted = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{member_id}"),
        Some(&owner_jar),
        Some(json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(promoted.status, StatusCode::OK, "{}", promoted.body);
    assert_eq!(
        promoted.body["role"].as_str(),
        Some("admin"),
        "{}",
        promoted.body
    );

    let invite_admin = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&admin_jar),
        Some(json!({ "email": unique_email(), "role": "admin" })),
    )
    .await;
    assert_eq!(
        invite_admin.status,
        StatusCode::FORBIDDEN,
        "{}",
        invite_admin.body
    );
    let msg = invite_admin.error().to_ascii_lowercase();
    assert!(
        msg.contains("admins can only invite members"),
        "expected admins-can-only-invite-members, got {:?}",
        invite_admin.error()
    );

    let invite_member = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&admin_jar),
        Some(json!({ "email": unique_email(), "role": "member" })),
    )
    .await;
    assert_eq!(
        invite_member.status,
        StatusCode::CREATED,
        "{}",
        invite_member.body
    );
    assert_eq!(invite_member.body["added_existing_user"], false);
}

#[tokio::test]
async fn admin_cannot_delete_owner_or_patch_roles() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
    let owner_id = user_id(&owner_login);
    let (member_email, admin_jar, member_login) = signup_and_login(&app).await;
    let admin_id = user_id(&member_login);

    let added = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": member_email, "role": "member" })),
    )
    .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);

    let promoted = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{admin_id}"),
        Some(&owner_jar),
        Some(json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(promoted.status, StatusCode::OK, "{}", promoted.body);

    let (other_email, _other_jar, other_login) = signup_and_login(&app).await;
    let other_id = user_id(&other_login);
    let other_added = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": other_email, "role": "member" })),
    )
    .await;
    assert_eq!(
        other_added.status,
        StatusCode::CREATED,
        "{}",
        other_added.body
    );

    let delete_owner = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/members/{owner_id}"),
        Some(&admin_jar),
        None,
    )
    .await;
    assert_eq!(
        delete_owner.status,
        StatusCode::FORBIDDEN,
        "{}",
        delete_owner.body
    );

    for (label, target) in [
        ("owner", owner_id.as_str()),
        ("admin-self", admin_id.as_str()),
        ("member", other_id.as_str()),
    ] {
        let patch = call(
            &app,
            "PATCH",
            &format!("/api/workspaces/{ws}/members/{target}"),
            Some(&admin_jar),
            Some(json!({ "role": "member" })),
        )
        .await;
        assert_eq!(
            patch.status,
            StatusCode::FORBIDDEN,
            "{label}: {}",
            patch.body
        );
        let msg = patch.error().to_ascii_lowercase();
        assert!(
            msg.contains("only an owner"),
            "{label}: expected only-an-owner, got {:?}",
            patch.error()
        );
    }

    let members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    let roles: Vec<(&str, &str)> = members.body["members"]
        .as_array()
        .expect("members")
        .iter()
        .filter_map(|m| Some((m["user_id"].as_str()?, m["role"].as_str()?)))
        .collect();
    assert!(
        roles
            .iter()
            .any(|(id, role)| *id == owner_id && *role == "owner"),
        "owner must remain owner: {roles:?}"
    );
}
