//! Persona id reject / isolation on GET/PATCH/DELETE detail routes.
//! Requires DATABASE_URL — never skips.

mod common;

use axum::http::StatusCode;
use serde_json::json;
use uuid::Uuid;

use common::*;

async fn signed_in_workspace(app: &axum::Router) -> (CookieJar, String) {
    let (_email, jar, login) = signup_and_login(app).await;
    let ws_id = login.body["default_workspace"]["id"]
        .as_str()
        .expect("default_workspace.id")
        .to_string();
    (jar, ws_id)
}

fn persona_uri(ws_id: &str, persona_id: &str) -> String {
    format!("/api/workspaces/{ws_id}/personas/{persona_id}")
}

/// Path<(Uuid, Uuid)> rejects a non-UUID persona id before the handler.
#[tokio::test]
async fn garbage_persona_id_is_400() {
    let app = app().await;
    let (jar, ws_id) = signed_in_workspace(&app).await;

    let out = call(
        &app,
        "PATCH",
        &persona_uri(&ws_id, "not-a-persona-id"),
        Some(&jar),
        Some(json!({ "is_default": true })),
    )
    .await;
    assert_eq!(out.status, StatusCode::BAD_REQUEST, "{}", out.body);
}

/// PATCH is_default=true on a missing UUID is 404 (no assign).
#[tokio::test]
async fn unknown_persona_id_is_rejected_on_assign_and_not_persisted() {
    let app = app().await;
    let (jar, ws_id) = signed_in_workspace(&app).await;
    let unknown = Uuid::now_v7();

    let out = call(
        &app,
        "PATCH",
        &persona_uri(&ws_id, &unknown.to_string()),
        Some(&jar),
        Some(json!({ "is_default": true })),
    )
    .await;
    assert_eq!(out.status, StatusCode::NOT_FOUND, "{}", out.body);
}

/// update() get-then-clear_default: a missing id must not clear the current default.
#[tokio::test]
async fn patch_unknown_persona_as_default_does_not_clear_existing_default() {
    let app = app().await;
    let (jar, ws_id) = signed_in_workspace(&app).await;

    let created = call(
        &app,
        "POST",
        &format!("/api/workspaces/{ws_id}/personas"),
        Some(&jar),
        Some(json!({ "name": "Existing default", "is_default": true })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let known_id = created.body["id"].as_str().expect("created id").to_string();
    assert_eq!(
        created.body["is_default"],
        true,
        "fixture must start as default: {}",
        created.body
    );

    let unknown = Uuid::now_v7();
    let out = call(
        &app,
        "PATCH",
        &persona_uri(&ws_id, &unknown.to_string()),
        Some(&jar),
        Some(json!({ "is_default": true })),
    )
    .await;
    assert_eq!(out.status, StatusCode::NOT_FOUND, "{}", out.body);

    let got = call(
        &app,
        "GET",
        &persona_uri(&ws_id, &known_id),
        Some(&jar),
        None,
    )
    .await;
    assert_eq!(got.status, StatusCode::OK, "{}", got.body);
    assert_eq!(
        got.body["is_default"],
        true,
        "existing default must stay set after failed assign: {}",
        got.body
    );
}

/// Member of B on /workspaces/{B}/personas/{A's id} is 404, not A's row.
#[tokio::test]
async fn get_patch_delete_persona_from_other_workspace_is_404() {
    let app = app().await;
    let (a_jar, a_ws) = signed_in_workspace(&app).await;
    let created = call(
        &app,
        "POST",
        &format!("/api/workspaces/{a_ws}/personas"),
        Some(&a_jar),
        Some(json!({ "name": "Workspace A persona" })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let a_pid = created.body["id"].as_str().expect("created id").to_string();

    let (b_jar, b_ws) = signed_in_workspace(&app).await;
    let uri = persona_uri(&b_ws, &a_pid);

    let got = call(&app, "GET", &uri, Some(&b_jar), None).await;
    assert_eq!(got.status, StatusCode::NOT_FOUND, "GET: {}", got.body);
    assert_ne!(
        got.body["id"].as_str(),
        Some(a_pid.as_str()),
        "GET must not return the other workspace's persona: {}",
        got.body
    );

    let patched = call(
        &app,
        "PATCH",
        &uri,
        Some(&b_jar),
        Some(json!({ "name": "injected-from-b", "is_default": true })),
    )
    .await;
    assert_eq!(patched.status, StatusCode::NOT_FOUND, "PATCH: {}", patched.body);

    let deleted = call(&app, "DELETE", &uri, Some(&b_jar), None).await;
    assert_eq!(deleted.status, StatusCode::NOT_FOUND, "DELETE: {}", deleted.body);

    let still = call(&app, "GET", &persona_uri(&a_ws, &a_pid), Some(&a_jar), None).await;
    assert_eq!(still.status, StatusCode::OK, "{}", still.body);
    assert_eq!(still.body["name"].as_str(), Some("Workspace A persona"));
    assert_eq!(still.body["status"].as_str(), Some("active"));
}

/// Detail routes require a session cookie (unauthenticated_cookie_routes omits them).
#[tokio::test]
async fn unauthenticated_persona_detail_is_401() {
    let app = app().await;
    let uri = persona_uri(
        &Uuid::now_v7().to_string(),
        &Uuid::now_v7().to_string(),
    );

    let get = call(&app, "GET", &uri, None, None).await;
    assert_eq!(get.status, StatusCode::UNAUTHORIZED, "GET: {}", get.body);

    let patch = call(
        &app,
        "PATCH",
        &uri,
        None,
        Some(json!({ "is_default": true })),
    )
    .await;
    assert_eq!(
        patch.status,
        StatusCode::UNAUTHORIZED,
        "PATCH: {}",
        patch.body
    );

    let delete = call(&app, "DELETE", &uri, None, None).await;
    assert_eq!(
        delete.status,
        StatusCode::UNAUTHORIZED,
        "DELETE: {}",
        delete.body
    );
}
