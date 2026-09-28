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

/// Inviting an existing user only opens an invite. Join before treating them as a member.
async fn accept_workspace_invite(app: &axum::Router, invitee_jar: &CookieJar, invited: &HttpOut) {
    let token = invited.body["invite"]["token"]
        .as_str()
        .expect("invite.token");
    let accepted = call(
        app,
        "POST",
        "/api/invites/accept",
        Some(invitee_jar),
        Some(json!({ "token": token })),
    )
    .await;
    assert_eq!(
        accepted.status,
        StatusCode::OK,
        "accept invite: {}",
        accepted.body
    );
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
    let mut jar = CookieJar::default();
    out.absorb_into(&mut jar);
    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status, StatusCode::UNAUTHORIZED, "signup is not a login");
}

#[tokio::test]
async fn signup_writes_no_sessions_row() {
    let app = app().await;
    let email = unique_email();
    let out = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(out.status, StatusCode::CREATED, "{}", out.body);

    let uid = Uuid::parse_str(&user_id(&out)).expect("user.id uuid");
    let pool = connect_and_migrate().await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM auth_sessions WHERE user_id = $1")
        .bind(uid)
        .fetch_one(&pool)
        .await
        .expect("count auth_sessions");
    assert_eq!(n, 0, "signup must not write a session row for {uid}");
}

#[tokio::test]
async fn signup_response_user_email_matches_request() {
    let app = app().await;
    let email = unique_email();
    let out = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(out.status, StatusCode::CREATED, "{}", out.body);
    assert_eq!(
        out.body["user"]["email"].as_str(),
        Some(email.as_str()),
        "{}",
        out.body
    );
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
    let first_id = Uuid::parse_str(&user_id(&first)).expect("user.id uuid");

    let second = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(second.status, StatusCode::CONFLICT, "{}", second.body);

    let pool = connect_and_migrate().await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE email_normalized = $1")
        .bind(email.to_ascii_lowercase())
        .fetch_one(&pool)
        .await
        .expect("count users");
    assert_eq!(n, 1, "duplicate signup must leave one users.email_normalized row");
    let leftover_id: Uuid =
        sqlx::query_scalar("SELECT id FROM users WHERE email_normalized = $1")
            .bind(email.to_ascii_lowercase())
            .fetch_one(&pool)
            .await
            .expect("remaining user id");
    assert_eq!(leftover_id, first_id, "the remaining row must be the first user");
}

#[tokio::test]
async fn signup_duplicate_mixed_case_email_409() {
    let app = app().await;
    let mixed = format!("Auth.DUP.{}@Example.TEST", Uuid::new_v4());
    let first_mixed = signup(&app, &mixed, TEST_PASSWORD).await;
    assert_eq!(
        first_mixed.status,
        StatusCode::CREATED,
        "{}",
        first_mixed.body
    );
    let again_lower = signup(&app, &mixed.to_ascii_lowercase(), TEST_PASSWORD).await;
    assert_eq!(
        again_lower.status,
        StatusCode::CONFLICT,
        "email uniqueness is case-insensitive: {}",
        again_lower.body
    );
}

#[tokio::test]
async fn signup_duplicate_409_does_not_set_session_cookies() {
    let app = app().await;
    let email = unique_email();
    let first = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);

    let second = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(second.status, StatusCode::CONFLICT, "{}", second.body);
    assert!(
        second.cookie(ACCESS_COOKIE).is_none(),
        "duplicate signup must not set {ACCESS_COOKIE}: {:?}",
        second.set_cookies()
    );
    assert!(
        second.cookie(REFRESH_COOKIE).is_none(),
        "duplicate signup must not set {REFRESH_COOKIE}: {:?}",
        second.set_cookies()
    );
}

#[tokio::test]
async fn signup_duplicate_trims_before_uniqueness() {
    let app = app().await;
    let email = unique_email();
    let first = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);

    let padded = format!(" {email} ");
    let second = signup(&app, &padded, TEST_PASSWORD).await;
    assert_eq!(
        second.status,
        StatusCode::CONFLICT,
        "signup must trim before uniqueness: {}",
        second.body
    );
}

#[tokio::test]
async fn signup_duplicate_leaves_single_account() {
    let app = app().await;
    let email = unique_email();
    let first = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(first.status, StatusCode::CREATED, "{}", first.body);

    let second = signup(&app, &email, "other-correct-horse").await;
    assert_eq!(second.status, StatusCode::CONFLICT, "{}", second.body);

    let (login_out, _) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(login_out.status, StatusCode::OK, "{}", login_out.body);

    let pool = connect_and_migrate().await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE email_normalized = $1")
        .bind(email.to_ascii_lowercase())
        .fetch_one(&pool)
        .await
        .expect("count users");
    assert_eq!(
        n, 1,
        "after 409, users.email_normalized must have one row for {email}"
    );
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

    let mut rotated = jar.clone();
    out.absorb_into(&mut rotated);
    assert_ne!(
        rotated.access(),
        Some(old_access.as_str()),
        "refresh must issue a new {ACCESS_COOKIE}"
    );
    assert_ne!(
        rotated.refresh(),
        Some(old_refresh.as_str()),
        "refresh must issue a new {REFRESH_COOKIE}"
    );

    let me = call(&app, "GET", "/api/auth/me", Some(&rotated), None).await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.body);

    let mut stale = CookieJar::default();
    stale.insert(ACCESS_COOKIE, old_access);
    let stale_me = call(&app, "GET", "/api/auth/me", Some(&stale), None).await;
    assert_eq!(
        stale_me.status,
        StatusCode::UNAUTHORIZED,
        "old access JWT must not work after rotation: {}",
        stale_me.body
    );
}

#[tokio::test]
async fn refresh_expired_session_is_401() {
    let app = app().await;
    let (_email, jar, login_out) = signup_and_login(&app).await;
    let uid = Uuid::parse_str(&user_id(&login_out)).expect("user.id uuid");

    let pool = connect_and_migrate().await;
    let n = sqlx::query(
        "UPDATE auth_sessions SET expires_at = now() - interval '1 minute' WHERE user_id = $1 AND revoked_at IS NULL",
    )
    .bind(uid)
    .execute(&pool)
    .await
    .expect("age expires_at");
    assert_eq!(n.rows_affected(), 1, "expected one session aged");

    let out = call(&app, "POST", "/api/auth/refresh", Some(&jar), None).await;
    assert_eq!(out.status, StatusCode::UNAUTHORIZED, "{}", out.body);
    assert_eq!(out.error(), "refresh token expired");
}

#[tokio::test]
async fn inactive_user_cannot_refresh() {
    let app = app().await;
    let (email, jar, _) = signup_and_login(&app).await;

    let pool = connect_and_migrate().await;
    let n = sqlx::query("UPDATE users SET status = 'disabled' WHERE email_normalized = $1")
        .bind(email.to_ascii_lowercase())
        .execute(&pool)
        .await
        .expect("deactivate user");
    assert_eq!(n.rows_affected(), 1, "expected one user row updated");

    let out = call(&app, "POST", "/api/auth/refresh", Some(&jar), None).await;
    assert_eq!(out.status, StatusCode::UNAUTHORIZED, "{}", out.body);
    assert_eq!(out.error(), "account is not active");
}

#[tokio::test]
async fn refresh_returns_default_workspace() {
    let app = app().await;
    let (_email, jar, _) = signup_and_login(&app).await;

    let out = call(&app, "POST", "/api/auth/refresh", Some(&jar), None).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    assert_eq!(
        out.body["message"].as_str(),
        Some("token refreshed"),
        "{}",
        out.body
    );
    assert_eq!(
        out.body["default_workspace"]["role"].as_str(),
        Some("owner"),
        "{}",
        out.body
    );
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
async fn create_workspace_slug_and_owner() {
    let app = app().await;
    let (_email, jar, _) = signup_and_login(&app).await;

    let out = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "name": "Acme Eng", "slug": "custom-lab" })),
    )
    .await;
    assert_eq!(out.status, StatusCode::CREATED, "{}", out.body);
    assert_eq!(out.body["workspace"]["role"], "owner");
    assert_eq!(out.body["workspace"]["name"], "Acme Eng");
    let id = Uuid::parse_str(
        out.body["workspace"]["id"]
            .as_str()
            .expect("workspace.id"),
    )
    .expect("workspace.id uuid");
    let slug = out.body["workspace"]["slug"]
        .as_str()
        .expect("workspace.slug");
    assert_eq!(
        slug,
        format!("custom-lab-{}", &id.to_string()[..8]),
        "slug must be slugify(slug field)+id[:8], not name fallback or suffix-drop"
    );
}

#[tokio::test]
async fn create_workspace_omitted_slug_derives_from_name() {
    let app = app().await;
    let (_email, jar, _) = signup_and_login(&app).await;

    let out = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "name": "Acme Eng" })),
    )
    .await;
    assert_eq!(out.status, StatusCode::CREATED, "{}", out.body);
    let slug = out.body["workspace"]["slug"]
        .as_str()
        .expect("workspace.slug");
    assert!(
        slug.starts_with("acme-eng-"),
        "omitted slug must derive from name, got {slug:?}"
    );
}

#[tokio::test]
async fn create_workspace_whitespace_name_is_400() {
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
    assert!(
        empty.error().contains("required"),
        "whitespace-only name should be required, got {:?}",
        empty.error()
    );
}

#[tokio::test]
async fn create_workspace_name_too_long_is_400() {
    let app = app().await;
    let (_email, jar, _) = signup_and_login(&app).await;

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
    assert!(
        too_long.error().contains("too long"),
        "81-byte name should be too long, got {:?}",
        too_long.error()
    );
}

#[tokio::test]
async fn create_workspace_name_of_exactly_80_is_201() {
    let app = app().await;
    let (_email, jar, _) = signup_and_login(&app).await;
    let name = "n".repeat(80);

    let out = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "name": name })),
    )
    .await;
    assert_eq!(out.status, StatusCode::CREATED, "{}", out.body);
    assert_eq!(
        out.body["workspace"]["name"].as_str(),
        Some(name.as_str()),
        "{}",
        out.body
    );
}

#[tokio::test]
async fn create_workspace_reject_does_not_list_new_workspace() {
    let app = app().await;
    let (_email, jar, login_out) = signup_and_login(&app).await;
    let default_id = workspace_id(&login_out);

    let rejected = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "name": "   " })),
    )
    .await;
    assert_eq!(
        rejected.status,
        StatusCode::BAD_REQUEST,
        "{}",
        rejected.body
    );

    let listed = call(&app, "GET", "/api/workspaces", Some(&jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids = listed_workspace_ids(&listed);
    assert_eq!(
        ids.as_slice(),
        [default_id.as_str()],
        "whitespace 400 must not insert a workspace: {ids:?}"
    );
}

#[tokio::test]
async fn create_workspace_omitted_name_is_not_201() {
    let app = app().await;
    let (_email, jar, _) = signup_and_login(&app).await;

    let omitted = call(
        &app,
        "POST",
        "/api/workspaces",
        Some(&jar),
        Some(json!({ "slug": "x" })),
    )
    .await;
    assert_ne!(
        omitted.status,
        StatusCode::CREATED,
        "omitted name must not create a workspace: {}",
        omitted.body
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
    let token = Uuid::new_v4();
    let stored = format!("Case.{token}@Example.TEST");
    let login_email = format!("CASE.{token}@eXaMpLe.TeSt");
    assert_ne!(
        login_email, stored,
        "login email must not equal the stored users.email column"
    );
    assert_ne!(
        login_email,
        stored.to_ascii_lowercase(),
        "login email must not equal users.email_normalized"
    );

    let created = signup(&app, &stored, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let signup_id = user_id(&created);

    let (out, jar) = login(&app, &login_email, TEST_PASSWORD).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    assert!(jar.access().is_some());

    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.body);
    assert_eq!(
        user_id(&me),
        signup_id,
        "third-casing login must resolve the signup user"
    );
}

#[tokio::test]
async fn login_trims_surrounding_whitespace() {
    let app = app().await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let signup_id = user_id(&created);

    let (out, jar) = login(&app, &format!(" {email} "), TEST_PASSWORD).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    assert!(jar.access().is_some());

    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.body);
    assert_eq!(
        user_id(&me),
        signup_id,
        "trimmed login must resolve the signup user"
    );
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
async fn member_invite_forbidden_does_not_insert_invite() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_me) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_me);

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

    let guest = unique_email();
    let forbidden = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&member_jar),
        Some(json!({ "email": guest, "role": "member" })),
    )
    .await;
    assert_eq!(
        forbidden.status,
        StatusCode::FORBIDDEN,
        "{}",
        forbidden.body
    );

    let listed = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let emails: Vec<&str> = listed.body["invites"]
        .as_array()
        .expect("invites")
        .iter()
        .filter_map(|i| i["email"].as_str())
        .collect();
    assert!(
        !emails.iter().any(|e| e.eq_ignore_ascii_case(&guest)),
        "member 403 must not insert an invite for {guest}: {emails:?}"
    );
}

#[tokio::test]
async fn invite_adds_existing_user_ignoring_email_case() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_me) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_me);

    let (member_email, _member_jar, _) = signup_and_login(&app).await;
    let mixed = member_email.to_ascii_uppercase();
    assert_ne!(
        mixed, member_email,
        "invite email must differ in case from the stored address"
    );

    let invited = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": mixed, "role": "member" })),
    )
    .await;
    assert_eq!(invited.status, StatusCode::CREATED, "{}", invited.body);
    assert_eq!(invited.body["added_existing_user"], true);
    assert!(invited.body["invite"]["token"].as_str().is_some());
}

#[tokio::test]
async fn pending_invite_joins_on_signup() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_me) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_me);

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

    let created = signup(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let token = inv.body["invite"]["token"]
        .as_str()
        .expect("create invite returns token once")
        .to_string();
    let (guest_login, guest_jar) = login(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(guest_login.status, StatusCode::OK, "{}", guest_login.body);
    let accepted = call(
        &app,
        "POST",
        "/api/invites/accept",
        Some(&guest_jar),
        Some(json!({ "token": token })),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.body);

    let members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    let guest_row = members.body["members"]
        .as_array()
        .expect("members")
        .iter()
        .find(|m| {
            m["email"]
                .as_str()
                .is_some_and(|e| e.eq_ignore_ascii_case(&guest))
        })
        .unwrap_or_else(|| panic!("guest must be a member after token accept: {}", members.body));
    assert_eq!(
        guest_row["role"].as_str(),
        Some("admin"),
        "guest should join as invited role: {}",
        members.body
    );

    let listed = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let emails: Vec<&str> = listed.body["invites"]
        .as_array()
        .expect("invites")
        .iter()
        .filter_map(|i| i["email"].as_str())
        .collect();
    assert!(
        !emails.iter().any(|e| e.eq_ignore_ascii_case(&guest)),
        "accepted invite must not remain on GET /invites after signup: {emails:?}"
    );
}

#[tokio::test]
async fn rejected_signup_does_not_consume_pending_invite() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_me) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_me);
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

    let rejected = signup(&app, &guest, "short").await;
    assert_eq!(rejected.status, StatusCode::BAD_REQUEST, "{}", rejected.body);

    let listed = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let emails: Vec<&str> = listed.body["invites"]
        .as_array()
        .expect("invites")
        .iter()
        .filter_map(|i| i["email"].as_str())
        .collect();
    assert!(
        emails.iter().any(|e| e.eq_ignore_ascii_case(&guest)),
        "rejected signup must leave the pending invite: {emails:?}"
    );
}

#[tokio::test]
async fn signup_joins_every_pending_workspace_before_login() {
    let app = app().await;
    let (_owner_a_email, owner_a_jar, owner_a) = signup_and_login(&app).await;
    let (_owner_b_email, owner_b_jar, owner_b) = signup_and_login(&app).await;
    let ws_a = workspace_id(&owner_a);
    let ws_b = workspace_id(&owner_b);
    let guest = unique_email();

    let inv_a = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws_a}/invites"),
        Some(&owner_a_jar),
        Some(json!({ "email": guest, "role": "admin" })),
    )
    .await;
    assert_eq!(inv_a.status, StatusCode::CREATED, "{}", inv_a.body);
    assert_eq!(inv_a.body["added_existing_user"], false);

    let inv_b = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws_b}/invites"),
        Some(&owner_b_jar),
        Some(json!({ "email": guest, "role": "member" })),
    )
    .await;
    assert_eq!(inv_b.status, StatusCode::CREATED, "{}", inv_b.body);
    assert_eq!(inv_b.body["added_existing_user"], false);

    let created = signup(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let (guest_login, guest_jar) = login(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(guest_login.status, StatusCode::OK, "{}", guest_login.body);
    for inv in [&inv_a, &inv_b] {
        let token = inv.body["invite"]["token"].as_str().unwrap();
        let accepted = call(
            &app,
            "POST",
            "/api/invites/accept",
            Some(&guest_jar),
            Some(json!({ "token": token })),
        )
        .await;
        assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.body);
    }

    for (jar, ws, role) in [
        (&owner_a_jar, &ws_a, "admin"),
        (&owner_b_jar, &ws_b, "member"),
    ] {
        let members = call(
            &app,
            "GET",
            &format!("/api/workspaces/{ws}/members"),
            Some(jar),
            None,
        )
        .await;
        assert_eq!(members.status, StatusCode::OK, "{}", members.body);
        let guest_row = members.body["members"]
            .as_array()
            .expect("members")
            .iter()
            .find(|m| {
                m["email"]
                    .as_str()
                    .is_some_and(|e| e.eq_ignore_ascii_case(&guest))
            })
            .unwrap_or_else(|| {
                panic!("guest must join {ws} as {role} after signup, before login: {}", members.body)
            });
        assert_eq!(
            guest_row["role"].as_str(),
            Some(role),
            "guest should join {ws} as invited role {role}: {}",
            members.body
        );
    }
}

#[tokio::test]
async fn signup_keeps_personal_workspace_and_invited() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_me) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_me);
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

    let created = signup(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let personal = created.body["default_workspace"]["id"]
        .as_str()
        .expect("signup default_workspace.id")
        .to_string();

    let (login_out, guest_jar) = login(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(login_out.status, StatusCode::OK, "{}", login_out.body);
    let token = inv.body["invite"]["token"].as_str().unwrap();
    let accepted = call(
        &app,
        "POST",
        "/api/invites/accept",
        Some(&guest_jar),
        Some(json!({ "token": token })),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.body);

    let listed = call(&app, "GET", "/api/workspaces", Some(&guest_jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids = listed_workspace_ids(&listed);
    assert!(
        ids.contains(&personal.as_str()),
        "guest first GET /workspaces must keep personal workspace {personal}: {ids:?}"
    );
    assert!(
        ids.contains(&ws.as_str()),
        "guest first GET /workspaces must include invited workspace {ws}: {ids:?}"
    );
}

#[tokio::test]
async fn pending_invite_joins_when_signup_email_case_differs() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_me) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_me);

    let guest = format!("Case.{}@Example.TEST", Uuid::new_v4());
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

    let lower = guest.to_ascii_lowercase();
    let created = signup(&app, &lower, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let (login_out, guest_jar) = login(&app, &lower, TEST_PASSWORD).await;
    assert_eq!(login_out.status, StatusCode::OK, "{}", login_out.body);
    let token = inv.body["invite"]["token"].as_str().unwrap();
    let accepted = call(
        &app,
        "POST",
        "/api/invites/accept",
        Some(&guest_jar),
        Some(json!({ "token": token })),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.body);

    let listed = call(&app, "GET", "/api/workspaces", Some(&guest_jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids = listed_workspace_ids(&listed);
    assert!(
        ids.contains(&ws.as_str()),
        "token accept must join after case-normalized signup: {ids:?}"
    );
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
        !before_emails.iter().any(|e| e.eq_ignore_ascii_case(&guest)),
        "pending invite must not add the user before login: {before_emails:?}"
    );

    let (login_out, guest_jar) = login(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(login_out.status, StatusCode::OK, "{}", login_out.body);
    let token = inv.body["invite"]["token"].as_str().unwrap();
    let accepted = call(
        &app,
        "POST",
        "/api/invites/accept",
        Some(&guest_jar),
        Some(json!({ "token": token })),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.body);

    let listed = call(&app, "GET", "/api/workspaces", Some(&guest_jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids = listed_workspace_ids(&listed);
    assert!(
        ids.contains(&ws.as_str()),
        "token accept must join workspace: {ids:?}"
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
    let personal = created.body["default_workspace"]["id"]
        .as_str()
        .expect("signup default_workspace.id")
        .to_string();

    let members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    let member_emails: Vec<&str> = members.body["members"]
        .as_array()
        .expect("members")
        .iter()
        .filter_map(|m| m["email"].as_str())
        .collect();
    assert!(
        !member_emails.iter().any(|e| e.eq_ignore_ascii_case(&guest)),
        "expired invite must not add guest on signup, before login: {member_emails:?}"
    );

    let (login_out, guest_jar) = login(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(login_out.status, StatusCode::OK, "{}", login_out.body);

    let listed = call(&app, "GET", "/api/workspaces", Some(&guest_jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids = listed_workspace_ids(&listed);
    assert!(
        ids.contains(&personal.as_str()),
        "guest GET /workspaces must keep personal workspace {personal}: {ids:?}"
    );
    assert!(
        !ids.contains(&ws.as_str()),
        "expired invite must not grant the workspace on signup: {ids:?} contains {ws}"
    );
}

#[tokio::test]
async fn expired_invite_does_not_join_on_login() {
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
    let invite_id = inv.body["invite"]["id"]
        .as_str()
        .expect("invite.id")
        .to_string();

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

    let aged = sqlx::query(
        "UPDATE workspace_invites SET expires_at = now() - interval '1 minute' WHERE id = $1",
    )
    .bind(Uuid::parse_str(&invite_id).expect("invite uuid"))
    .execute(&pool)
    .await
    .expect("age expires_at");
    assert_eq!(aged.rows_affected(), 1, "expected one invite aged");

    let (login_out, guest_jar) = login(&app, &guest, TEST_PASSWORD).await;
    assert_eq!(login_out.status, StatusCode::OK, "{}", login_out.body);
    let personal = workspace_id(&login_out);

    let listed = call(&app, "GET", "/api/workspaces", Some(&guest_jar), None).await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids = listed_workspace_ids(&listed);
    assert!(
        ids.contains(&personal.as_str()),
        "guest GET /workspaces must keep personal workspace {personal}: {ids:?}"
    );
    assert!(
        !ids.contains(&ws.as_str()),
        "expired invite must not grant the workspace on login: {ids:?} contains {ws}"
    );

    let members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    let member_emails: Vec<&str> = members.body["members"]
        .as_array()
        .expect("members")
        .iter()
        .filter_map(|m| m["email"].as_str())
        .collect();
    assert!(
        !member_emails.iter().any(|e| e.eq_ignore_ascii_case(&guest)),
        "expired invite must not add guest on login: {member_emails:?}"
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
async fn member_cannot_list_invites() {
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
    let token = added.body["invite"]["token"].as_str().unwrap();
    let accepted = call(
        &app,
        "POST",
        "/api/invites/accept",
        Some(&member_jar),
        Some(json!({ "token": token })),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.body);

    let list = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&member_jar),
        None,
    )
    .await;
    assert_eq!(list.status, StatusCode::FORBIDDEN, "{}", list.body);
    assert_eq!(list.error(), "not allowed to list invites");
    assert!(
        list.body.get("invites").is_none(),
        "member 403 must not include an invites payload: {}",
        list.body
    );
}

#[tokio::test]
async fn member_cannot_revoke_invites() {
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
    let token = added.body["invite"]["token"].as_str().unwrap();
    let accepted = call(
        &app,
        "POST",
        "/api/invites/accept",
        Some(&member_jar),
        Some(json!({ "token": token })),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.body);

    let revoke = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/invites/{invite_id}"),
        Some(&member_jar),
        None,
    )
    .await;
    assert_eq!(revoke.status, StatusCode::FORBIDDEN, "{}", revoke.body);
    assert_eq!(revoke.error(), "not allowed to revoke invites");

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
async fn admin_can_list_and_revoke_invites() {
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
    assert_eq!(added.body["added_existing_user"], true);

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

    let listed = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&admin_jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let listed_ids: Vec<&str> = listed.body["invites"]
        .as_array()
        .expect("invites")
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    assert!(
        listed_ids.contains(&invite_id.as_str()),
        "admin GET /invites must include pending {invite_id}: {listed_ids:?}"
    );

    let revoked = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/invites/{invite_id}"),
        Some(&admin_jar),
        None,
    )
    .await;
    assert_eq!(revoked.status, StatusCode::OK, "{}", revoked.body);
    assert_eq!(revoked.body["revoked"], true);

    let still = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(still.status, StatusCode::OK, "{}", still.body);
    let leftover: Vec<&str> = still.body["invites"]
        .as_array()
        .expect("invites")
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    assert!(
        !leftover.contains(&invite_id.as_str()),
        "admin revoke must drop {invite_id} from owner GET: {leftover:?}"
    );
}

#[tokio::test]
async fn outsider_cannot_list_or_revoke_invites() {
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

    let (_outsider_email, outsider_jar, _) = signup_and_login(&app).await;

    let list = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&outsider_jar),
        None,
    )
    .await;
    assert_eq!(list.status, StatusCode::FORBIDDEN, "{}", list.body);
    assert!(
        list.error().contains("not a member"),
        "outsider list should say not a member, got {:?}",
        list.error()
    );
    assert!(
        list.body.get("invites").is_none(),
        "outsider 403 must not include an invites payload: {}",
        list.body
    );

    let revoke = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/invites/{invite_id}"),
        Some(&outsider_jar),
        None,
    )
    .await;
    assert_eq!(revoke.status, StatusCode::FORBIDDEN, "{}", revoke.body);
    assert!(
        revoke.error().contains("not a member"),
        "outsider revoke should say not a member, got {:?}",
        revoke.error()
    );

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
        "outsider revoke must not remove the invite: {ids:?}"
    );
}

#[tokio::test]
async fn unauthenticated_invites_are_401() {
    let app = app().await;
    let (_email, owner_jar, owner_login) = signup_and_login(&app).await;
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

    let list_uri = format!("/api/workspaces/{ws}/invites");
    let revoke_uri = format!("/api/workspaces/{ws}/invites/{invite_id}");
    for (method, uri) in [("GET", list_uri.as_str()), ("DELETE", revoke_uri.as_str())] {
        let out = call(&app, method, uri, None, None).await;
        assert_eq!(
            out.status,
            StatusCode::UNAUTHORIZED,
            "{method} {uri}: {}",
            out.body
        );
        assert_eq!(out.error(), "not authenticated");
    }
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
    let (member_email, member_jar, _) = signup_and_login(&app).await;

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
    let token = first.body["invite"]["token"].as_str().unwrap();
    let accepted = call(
        &app,
        "POST",
        "/api/invites/accept",
        Some(&member_jar),
        Some(json!({ "token": token })),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.body);

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
async fn owner_get_invites_omits_expired() {
    let app = app().await;
    let (_email, jar, login_out) = signup_and_login(&app).await;
    let ws = workspace_id(&login_out);

    let expired = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&jar),
        Some(json!({ "email": unique_email(), "role": "member" })),
    )
    .await;
    assert_eq!(expired.status, StatusCode::CREATED, "{}", expired.body);
    let expired_id = expired.body["invite"]["id"]
        .as_str()
        .expect("invite.id")
        .to_string();

    let live = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&jar),
        Some(json!({ "email": unique_email(), "role": "member" })),
    )
    .await;
    assert_eq!(live.status, StatusCode::CREATED, "{}", live.body);
    let live_id = live.body["invite"]["id"]
        .as_str()
        .expect("invite.id")
        .to_string();

    let pool = connect_and_migrate().await;
    let n = sqlx::query(
        "UPDATE workspace_invites SET expires_at = now() - interval '1 minute' WHERE id = $1",
    )
    .bind(Uuid::parse_str(&expired_id).expect("invite uuid"))
    .execute(&pool)
    .await
    .expect("age expires_at");
    assert_eq!(n.rows_affected(), 1, "expected one invite aged");

    let listed = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let ids: Vec<&str> = listed.body["invites"]
        .as_array()
        .expect("invites")
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    assert!(
        ids.contains(&live_id.as_str()),
        "owner GET /invites must still list the live invite {live_id}: {ids:?}"
    );
    assert!(
        !ids.contains(&expired_id.as_str()),
        "owner GET /invites must omit expired invite {expired_id}: {ids:?}"
    );
}

#[tokio::test]
async fn admin_invite_admin_does_not_create_open_invite() {
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
    accept_workspace_invite(&app, &admin_jar, &added).await;

    let promoted = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{member_id}"),
        Some(&owner_jar),
        Some(json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(promoted.status, StatusCode::OK, "{}", promoted.body);

    let guest = unique_email();
    let invite_admin = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&admin_jar),
        Some(json!({ "email": guest, "role": "admin" })),
    )
    .await;
    assert_eq!(
        invite_admin.status,
        StatusCode::FORBIDDEN,
        "{}",
        invite_admin.body
    );

    let listed = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.body);
    let emails: Vec<&str> = listed.body["invites"]
        .as_array()
        .expect("invites")
        .iter()
        .filter_map(|i| i["email"].as_str())
        .collect();
    assert!(
        !emails.iter().any(|e| e.eq_ignore_ascii_case(&guest)),
        "admin 403 must not insert an invite for {guest}: {emails:?}"
    );
}

#[tokio::test]
async fn admin_invite_member_records_role_member() {
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
    accept_workspace_invite(&app, &admin_jar, &added).await;

    let promoted = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{member_id}"),
        Some(&owner_jar),
        Some(json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(promoted.status, StatusCode::OK, "{}", promoted.body);

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
    assert_eq!(
        invite_member.body["invite"]["role"].as_str(),
        Some("member"),
        "{}",
        invite_member.body
    );
}

#[tokio::test]
async fn admin_invite_existing_user_as_admin_is_403_and_not_added() {
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
    accept_workspace_invite(&app, &admin_jar, &added).await;

    let promoted = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{member_id}"),
        Some(&owner_jar),
        Some(json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(promoted.status, StatusCode::OK, "{}", promoted.body);

    let (guest_email, _guest_jar, _) = signup_and_login(&app).await;
    let invite_admin = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&admin_jar),
        Some(json!({ "email": guest_email, "role": "admin" })),
    )
    .await;
    assert_eq!(
        invite_admin.status,
        StatusCode::FORBIDDEN,
        "{}",
        invite_admin.body
    );

    let members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    let emails: Vec<&str> = members.body["members"]
        .as_array()
        .expect("members")
        .iter()
        .filter_map(|m| m["email"].as_str())
        .collect();
    assert!(
        !emails
            .iter()
            .any(|e| e.eq_ignore_ascii_case(&guest_email)),
        "admin 403 must not add existing user {guest_email}: {emails:?}"
    );
}

#[tokio::test]
async fn admin_cannot_delete_owner() {
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
    accept_workspace_invite(&app, &admin_jar, &added).await;

    let promoted = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{admin_id}"),
        Some(&owner_jar),
        Some(json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(promoted.status, StatusCode::OK, "{}", promoted.body);

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

    let members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    let owner_role = members.body["members"]
        .as_array()
        .expect("members")
        .iter()
        .find(|m| m["user_id"].as_str() == Some(owner_id.as_str()))
        .and_then(|m| m["role"].as_str());
    assert_eq!(
        owner_role,
        Some("owner"),
        "owner must remain owner after admin DELETE: {}",
        members.body
    );
}

#[tokio::test]
async fn admin_cannot_patch_owner_role() {
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
    accept_workspace_invite(&app, &admin_jar, &added).await;

    let promoted = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{admin_id}"),
        Some(&owner_jar),
        Some(json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(promoted.status, StatusCode::OK, "{}", promoted.body);

    let patched = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{owner_id}"),
        Some(&admin_jar),
        Some(json!({ "role": "member" })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::FORBIDDEN, "{}", patched.body);

    let members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    let owner_role = members.body["members"]
        .as_array()
        .expect("members")
        .iter()
        .find(|m| m["user_id"].as_str() == Some(owner_id.as_str()))
        .and_then(|m| m["role"].as_str());
    assert_eq!(
        owner_role,
        Some("owner"),
        "owner must remain owner after admin PATCH: {}",
        members.body
    );
}

#[tokio::test]
async fn admin_cannot_patch_self_role() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
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
    accept_workspace_invite(&app, &admin_jar, &added).await;

    let promoted = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{admin_id}"),
        Some(&owner_jar),
        Some(json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(promoted.status, StatusCode::OK, "{}", promoted.body);

    let patched = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{admin_id}"),
        Some(&admin_jar),
        Some(json!({ "role": "member" })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::FORBIDDEN, "{}", patched.body);

    let members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    let admin_role = members.body["members"]
        .as_array()
        .expect("members")
        .iter()
        .find(|m| m["user_id"].as_str() == Some(admin_id.as_str()))
        .and_then(|m| m["role"].as_str());
    assert_eq!(
        admin_role,
        Some("admin"),
        "admin must remain admin after self PATCH: {}",
        members.body
    );
}

#[tokio::test]
async fn admin_cannot_patch_member_role() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
    let (admin_email, admin_jar, admin_login) = signup_and_login(&app).await;
    let admin_id = user_id(&admin_login);

    let added = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": admin_email, "role": "member" })),
    )
    .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);
    accept_workspace_invite(&app, &admin_jar, &added).await;

    let promoted = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{admin_id}"),
        Some(&owner_jar),
        Some(json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(promoted.status, StatusCode::OK, "{}", promoted.body);

    let (other_email, other_jar, other_login) = signup_and_login(&app).await;
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
    accept_workspace_invite(&app, &other_jar, &other_added).await;

    let patched = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{other_id}"),
        Some(&admin_jar),
        Some(json!({ "role": "member" })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::FORBIDDEN, "{}", patched.body);

    let members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    let leftover: Vec<(&str, &str)> = members.body["members"]
        .as_array()
        .expect("members")
        .iter()
        .filter_map(|m| Some((m["user_id"].as_str()?, m["role"].as_str()?)))
        .collect();
    assert!(
        leftover
            .iter()
            .any(|(id, role)| *id == admin_id && *role == "admin"),
        "admin role must be unchanged after PATCH: {leftover:?}"
    );
    assert!(
        leftover
            .iter()
            .any(|(id, role)| *id == other_id && *role == "member"),
        "member role must be unchanged after PATCH: {leftover:?}"
    );
}

#[tokio::test]
async fn admin_cannot_promote_self_to_owner() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
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
    accept_workspace_invite(&app, &admin_jar, &added).await;

    let promoted = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{admin_id}"),
        Some(&owner_jar),
        Some(json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(promoted.status, StatusCode::OK, "{}", promoted.body);

    let patched = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{admin_id}"),
        Some(&admin_jar),
        Some(json!({ "role": "owner" })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::FORBIDDEN, "{}", patched.body);

    let members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&admin_jar),
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    assert_eq!(
        members.body["you"]["role"].as_str(),
        Some("admin"),
        "admin PATCH self role=owner must leave you.role admin: {}",
        members.body
    );
}

#[tokio::test]
async fn admin_can_delete_non_owner_member() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
    let (admin_email, admin_jar, admin_login) = signup_and_login(&app).await;
    let admin_id = user_id(&admin_login);

    let added = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": admin_email, "role": "member" })),
    )
    .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);
    accept_workspace_invite(&app, &admin_jar, &added).await;

    let promoted = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{admin_id}"),
        Some(&owner_jar),
        Some(json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(promoted.status, StatusCode::OK, "{}", promoted.body);

    let (other_email, other_jar, other_login) = signup_and_login(&app).await;
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
    accept_workspace_invite(&app, &other_jar, &other_added).await;

    let deleted = call(
        &app,
        "DELETE",
        &format!("/api/workspaces/{ws}/members/{other_id}"),
        Some(&admin_jar),
        None,
    )
    .await;
    assert_eq!(deleted.status, StatusCode::OK, "{}", deleted.body);
    assert_eq!(deleted.body["removed"], true, "{}", deleted.body);

    let members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    let leftover_ids: Vec<&str> = members.body["members"]
        .as_array()
        .expect("members")
        .iter()
        .filter_map(|m| m["user_id"].as_str())
        .collect();
    assert!(
        !leftover_ids.contains(&other_id.as_str()),
        "admin DELETE must omit {other_id}: {leftover_ids:?}"
    );
}

#[tokio::test]
async fn member_cannot_patch_peer_role() {
    let app = app().await;
    let (_owner_email, owner_jar, owner_login) = signup_and_login(&app).await;
    let ws = workspace_id(&owner_login);
    let (member_email, member_jar, _member_login) = signup_and_login(&app).await;
    let added = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": member_email, "role": "member" })),
    )
    .await;
    assert_eq!(added.status, StatusCode::CREATED, "{}", added.body);
    accept_workspace_invite(&app, &member_jar, &added).await;

    let (peer_email, peer_jar, peer_login) = signup_and_login(&app).await;
    let peer_id = user_id(&peer_login);
    let peer_added = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws}/invites"),
        Some(&owner_jar),
        Some(json!({ "email": peer_email, "role": "member" })),
    )
    .await;
    assert_eq!(
        peer_added.status,
        StatusCode::CREATED,
        "{}",
        peer_added.body
    );
    accept_workspace_invite(&app, &peer_jar, &peer_added).await;

    let patched = call(
        &app,
        "PATCH",
        &format!("/api/workspaces/{ws}/members/{peer_id}"),
        Some(&member_jar),
        Some(json!({ "role": "admin" })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::FORBIDDEN, "{}", patched.body);

    let members = call(
        &app,
        "GET",
        &format!("/api/workspaces/{ws}/members"),
        Some(&owner_jar),
        None,
    )
    .await;
    assert_eq!(members.status, StatusCode::OK, "{}", members.body);
    let peer_role = members.body["members"]
        .as_array()
        .expect("members")
        .iter()
        .find(|m| m["user_id"].as_str() == Some(peer_id.as_str()))
        .and_then(|m| m["role"].as_str());
    assert_eq!(
        peer_role,
        Some("member"),
        "peer must remain member after member PATCH: {}",
        members.body
    );
}

#[tokio::test]
async fn login_rate_limit_uses_real_ip_not_spoofed_xff() {
    // Host .env may raise AUTH_LOGIN_RATE_MAX for browser e2e. This lock
    // is the product default (10 / 15m); do not inherit the lab cap.
    let previous = std::env::var("AUTH_LOGIN_RATE_MAX").ok();
    unsafe { std::env::set_var("AUTH_LOGIN_RATE_MAX", "10") };
    struct Restore(Option<String>);
    impl Drop for Restore {
        fn drop(&mut self) {
            unsafe {
                match &self.0 {
                    Some(v) => std::env::set_var("AUTH_LOGIN_RATE_MAX", v),
                    None => std::env::remove_var("AUTH_LOGIN_RATE_MAX"),
                }
            }
        }
    }
    let _restore = Restore(previous);
    let app = app().await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let real = format!("203.0.113.{}", (Uuid::new_v4().as_u128() % 200) + 1);
    let body = json!({ "email": email, "password": "wrong-password-xx" });
    for i in 0..10 {
        let spoof = format!("198.51.100.{i}");
        let out = call_with_proxy_ips(
            &app,
            "POST",
            "/api/auth/login",
            &spoof,
            &real,
            Some(body.clone()),
        )
        .await;
        assert_eq!(
            out.status,
            StatusCode::UNAUTHORIZED,
            "attempt {i}: {}",
            out.body
        );
    }
    let blocked = call_with_proxy_ips(
        &app,
        "POST",
        "/api/auth/login",
        "198.51.100.99",
        &real,
        Some(body),
    )
    .await;
    assert_eq!(
        blocked.status,
        StatusCode::TOO_MANY_REQUESTS,
        "{}",
        blocked.body
    );
}

#[tokio::test]
async fn signup_requires_token_when_configured() {
    let app = app_with_signup_token("shared-invite").await;
    let email = unique_email();
    let missing = signup_with_token(&app, &email, TEST_PASSWORD, None).await;
    assert_eq!(missing.status, StatusCode::FORBIDDEN, "{}", missing.body);

    let (login_out, _) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(login_out.status, StatusCode::UNAUTHORIZED, "{}", login_out.body);
}

#[tokio::test]
async fn signup_accepts_matching_token_when_configured() {
    let app = app_with_signup_token("shared-invite").await;
    let ok = signup_with_token(&app, &unique_email(), TEST_PASSWORD, Some("shared-invite")).await;
    assert_eq!(ok.status, StatusCode::CREATED, "{}", ok.body);
}

#[tokio::test]
async fn signup_bad_token_does_not_insert_user() {
    let app = app_with_signup_token("shared-invite").await;
    let email = unique_email();
    let wrong = signup_with_token(&app, &email, TEST_PASSWORD, Some("nope")).await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN, "{}", wrong.body);

    let (login_out, _) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(login_out.status, StatusCode::UNAUTHORIZED, "{}", login_out.body);
}

#[tokio::test]
async fn signup_accepts_submitted_token_with_surrounding_whitespace() {
    let app = app_with_signup_token("shared-invite").await;
    let ok =
        signup_with_token(&app, &unique_email(), TEST_PASSWORD, Some("  shared-invite  ")).await;
    assert_eq!(ok.status, StatusCode::CREATED, "{}", ok.body);
}

#[tokio::test]
async fn signup_rejects_case_mismatched_token() {
    let app = app_with_signup_token("shared-invite").await;
    let out = signup_with_token(&app, &unique_email(), TEST_PASSWORD, Some("Shared-Invite")).await;
    assert_eq!(out.status, StatusCode::FORBIDDEN, "{}", out.body);
}
