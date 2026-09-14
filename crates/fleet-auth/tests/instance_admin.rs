//! INSTANCE_ADMIN_EMAILS grant on signup/login/me.

use serde_json::json;

use common::{
    TEST_PASSWORD, app, app_with_instance_admin_emails, call, login, signup, unique_email,
};

mod common;

#[tokio::test]
async fn me_instance_admin_false_by_default() {
    let app = app().await;
    let email = unique_email();
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status.as_u16(), 201, "{}", created.body);
    let (login_out, jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(login_out.status.as_u16(), 200, "{}", login_out.body);
    assert_eq!(login_out.body["user"]["instance_admin"], false);
    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.status.as_u16(), 200, "{}", me.body);
    assert_eq!(me.body["user"]["instance_admin"], false);
}

#[tokio::test]
async fn env_email_is_not_granted_on_signup_or_login() {
    let email = unique_email();
    let app = app_with_instance_admin_emails(&[email.as_str()]).await;
    let created = signup(&app, &email, TEST_PASSWORD).await;
    assert_eq!(created.status.as_u16(), 201, "{}", created.body);
    assert_eq!(
        created.body["user"]["instance_admin"], false,
        "signup must not grant instance admin from env: {}",
        created.body
    );
    let (login_out, jar) = login(&app, &email, TEST_PASSWORD).await;
    assert_eq!(login_out.status.as_u16(), 200, "{}", login_out.body);
    assert_eq!(login_out.body["user"]["instance_admin"], false);
    let me = call(&app, "GET", "/api/auth/me", Some(&jar), None).await;
    assert_eq!(me.body["user"]["instance_admin"], false, "{}", me.body);
}

#[tokio::test]
async fn env_email_is_not_granted_case_insensitive() {
    let local = unique_email();
    let (user, domain) = local.split_once('@').expect("email");
    let mixed = format!("{}@{}", user.to_uppercase(), domain);
    let app = app_with_instance_admin_emails(&[mixed.as_str()]).await;
    let created = signup(&app, &local, TEST_PASSWORD).await;
    assert_eq!(created.status.as_u16(), 201, "{}", created.body);
    assert_eq!(created.body["user"]["instance_admin"], false, "{}", created.body);
}

#[tokio::test]
async fn other_email_is_not_granted() {
    let listed = unique_email();
    let app = app_with_instance_admin_emails(&[listed.as_str()]).await;
    let other = unique_email();
    let created = signup(&app, &other, TEST_PASSWORD).await;
    assert_eq!(created.status.as_u16(), 201, "{}", created.body);
    assert_eq!(created.body["user"]["instance_admin"], false, "{}", created.body);
}

#[tokio::test]
async fn admin_routes_are_not_on_auth_router() {
    let app = app().await;
    let email = unique_email();
    let _ = signup(&app, &email, TEST_PASSWORD).await;
    let (_out, jar) = login(&app, &email, TEST_PASSWORD).await;
    let got = call(&app, "GET", "/api/admin/overview", Some(&jar), None).await;
    // fleet-auth router does not mount /api/admin — 404, not a silent allow.
    assert_eq!(
        got.status.as_u16(),
        404,
        "auth crate must not serve /api/admin: {}",
        got.body
    );
    let _ = json!({});
}
