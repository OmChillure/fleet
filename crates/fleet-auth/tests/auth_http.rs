//! Fail-closed HTTP integration tests for fleet-auth (in-process Axum oneshot).
//! Requires DATABASE_URL — never skips.

mod common;

use axum::http::StatusCode;
use fleet_auth::{ACCESS_COOKIE, REFRESH_COOKIE};
use serde_json::json;

use common::*;

#[tokio::test]
async fn signup_201_no_session_cookies() {
    let app = app().await;
    let email = unique_email();
    let out = signup(&app, &email, TEST_PASSWORD).await;

    assert_eq!(out.status, StatusCode::CREATED, "{}", out.body);
    let message = out.body["message"].as_str().unwrap_or("");
    assert!(
        message.contains("log in"),
        "expected message to mention log in, got {message:?}"
    );
    assert_eq!(out.body["default_workspace"]["role"], "owner");
    assert!(
        out.cookie(ACCESS_COOKIE).is_none(),
        "signup must not set {ACCESS_COOKIE}: {:?}",
        out.set_cookies()
    );
}

#[tokio::test]
async fn signup_rejects_bad_email() {
    let app = app().await;
    let out = signup(&app, "not-an-email", TEST_PASSWORD).await;
    assert_eq!(out.status, StatusCode::BAD_REQUEST, "{}", out.body);
}

#[tokio::test]
async fn signup_rejects_short_password() {
    let app = app().await;
    let out = signup(&app, &unique_email(), "short").await;
    assert_eq!(out.status, StatusCode::BAD_REQUEST, "{}", out.body);
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
}

#[tokio::test]
async fn login_sets_httponly_cookies_and_workspace() {
    let app = app().await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);

    let (out, jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(out.status, StatusCode::OK, "{}", out.body);
    assert_eq!(out.body["default_workspace"]["role"], "owner");

    let access = out
        .cookie(ACCESS_COOKIE)
        .unwrap_or_else(|| panic!("missing {ACCESS_COOKIE} Set-Cookie"));
    let refresh = out
        .cookie(REFRESH_COOKIE)
        .unwrap_or_else(|| panic!("missing {REFRESH_COOKIE} Set-Cookie"));
    assert!(
        access.is_httponly(),
        "{ACCESS_COOKIE} should be HttpOnly: {}",
        access.raw
    );
    assert!(
        refresh.is_httponly(),
        "{REFRESH_COOKIE} should be HttpOnly: {}",
        refresh.raw
    );
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
}
