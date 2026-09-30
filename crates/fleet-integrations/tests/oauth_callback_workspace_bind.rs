//! OAuth callback `state.workspace_id` must bind to that workspace only.
//! A callback with no `fleet_access` session must not complete Connect.
//! An expired OAuth `state` JWT must not complete Connect.
//! Callback / PUT oauth-app persist AES-GCM envelopes, never plaintext.
//! Drives the HTTP wrapper (`oauth_callback`) — not `oauth_callback_inner` alone.
//! Requires DATABASE_URL — never skips.

use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use chrono::Utc;
use fleet_auth::{ACCESS_COOKIE, REFRESH_COOKIE};
use fleet_db::NewAuthSession;
use fleet_db::repos::{integrations as integ_repo, sessions as session_repo, users, workspaces};
use fleet_integrations::{
    CredentialsKey, IntegrationsConfig, IntegrationsState, OAuthClientConfig,
};
use jsonwebtoken::{EncodingKey, Header, encode};
use serde::Serialize;
use tower::ServiceExt;
use uuid::Uuid;

fn require_database_url() -> String {
    std::env::var("DATABASE_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            panic!(
                "DATABASE_URL is required for this integration test. \
                 Start Postgres (just db-up) and export DATABASE_URL."
            )
        })
}

async fn connect_and_migrate() -> fleet_db::PgPool {
    let url = require_database_url();
    fleet_db::connect_and_migrate_with(
        &url,
        &fleet_db::PoolConfig {
            max_connections: 5,
            min_connections: 0,
            acquire_timeout: Duration::from_secs(10),
            idle_timeout: Some(Duration::from_secs(30)),
            max_lifetime: None,
        },
    )
    .await
    .unwrap_or_else(|e| panic!("connect_and_migrate failed: {e}"))
}

async fn seed_user_with_workspace(pool: &fleet_db::PgPool, label: &str) -> (Uuid, String, Uuid) {
    let user_id = Uuid::now_v7();
    let email = format!("oauth.ws.bind.{label}.{user_id}@example.test");
    let now = Utc::now();
    let mut tx = pool.begin().await.expect("begin user");
    users::insert_tx(
        &mut tx,
        &users::NewUser {
            id: user_id,
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
    let slug = format!("owb-{label}-{workspace_id}");
    workspaces::create_owned(
        pool,
        workspace_id,
        user_id,
        &slug,
        &format!("OAuth ws bind {label}"),
        now,
    )
    .await
    .expect("create workspace");
    (user_id, email, workspace_id)
}

fn test_state(pool: fleet_db::PgPool) -> IntegrationsState {
    IntegrationsState {
        db: pool,
        auth: fleet_auth::AuthConfig::from_env().expect("auth config"),
        config: IntegrationsConfig {
            api_public_base: "http://127.0.0.1:8080".into(),
            ui_public_base: "http://127.0.0.1:3030".into(),
            state_secret: "oauth-ws-bind-test-secret-32chars!!".into(),
            credentials_key: CredentialsKey::from_secret("dev-test-encryption-key-32chars!!")
                .expect("test key"),
            github: None,
            gitlab: None,
            gitea: None,
            gitea_base_url: "http://localhost:3033".into(),
            gogs_base_url: "http://localhost:3034".into(),
            slack: None,
            linear: None,
            jira: None,
            figma: None,
            notion: None,
            google_drive: None,
            teams: None,
            discord: None,
            trello: Some(OAuthClientConfig {
                client_id: "trello-test-key".into(),
                client_secret: "trello-test-secret".into(),
                github_app_slug: None,
            }),
            asana: None,
            bitbucket: None,
            confluence: None,
        },
    }
}

/// Signed state JWT with an explicit nonce, `typ`, and `iat`/`exp` offset.
/// `exp_offset_secs` is added to `iat` (negative → already expired).
fn encode_oauth_state(
    secret: &str,
    user_id: Uuid,
    workspace_id: Uuid,
    provider: &str,
    nonce: &str,
    iat: i64,
    exp_offset_secs: i64,
    typ: &str,
) -> String {
    #[derive(Serialize)]
    struct Claims {
        sub: String,
        workspace_id: String,
        provider: String,
        nonce: String,
        exp: i64,
        iat: i64,
        typ: String,
    }
    encode(
        &Header::default(),
        &Claims {
            sub: user_id.to_string(),
            workspace_id: workspace_id.to_string(),
            provider: provider.to_string(),
            nonce: nonce.to_string(),
            exp: iat + exp_offset_secs,
            iat,
            typ: typ.to_string(),
        },
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .expect("oauth state jwt")
}

async fn session_cookie(state: &IntegrationsState, user_id: Uuid, email: &str) -> String {
    let session_id = Uuid::now_v7();
    let now = Utc::now();
    session_repo::insert(
        &state.db,
        &NewAuthSession {
            id: session_id,
            user_id,
            refresh_token_hash: session_id.as_bytes().to_vec(),
            family_id: Uuid::now_v7(),
            user_agent: Some("oauth-ws-bind-test".into()),
            ip: None,
            expires_at: now + chrono::Duration::hours(1),
            absolute_expires_at: now + chrono::Duration::days(1),
        },
    )
    .await
    .expect("insert session");

    #[derive(Serialize)]
    struct AccessClaims {
        sub: String,
        email: String,
        sid: String,
        exp: i64,
        iat: i64,
    }
    let ts = now.timestamp();
    encode(
        &Header::default(),
        &AccessClaims {
            sub: user_id.to_string(),
            email: email.to_string(),
            sid: session_id.to_string(),
            exp: ts + 600,
            iat: ts,
        },
        &EncodingKey::from_secret(state.auth.jwt_secret.as_bytes()),
    )
    .expect("access jwt")
}

async fn creds_on(
    pool: &fleet_db::PgPool,
    workspace_id: Uuid,
    key: &CredentialsKey,
) -> Option<serde_json::Value> {
    fleet_integrations::credentials_for(pool, workspace_id, "trello", key)
        .await
        .expect("load creds")
}

/// Raw `credentials::text` from Postgres — not `credentials_for` (that
/// decrypts envelopes *and* passes through a raw `{access_token}` blob).
async fn credentials_text(
    pool: &fleet_db::PgPool,
    workspace_id: Uuid,
    provider: &str,
) -> String {
    sqlx::query_scalar(
        r#"
        SELECT credentials::text
        FROM integration_connections
        WHERE workspace_id = $1 AND provider = $2
        "#,
    )
    .bind(workspace_id)
    .bind(provider)
    .fetch_one(pool)
    .await
    .expect("SELECT credentials::text")
}

/// AES-256-GCM envelope written by `CredentialsKey::encrypt`. decrypt() of a
/// raw `{access_token}` blob still succeeds — this shape is the at-rest lock.
fn assert_aes_gcm_envelope(stored: &serde_json::Value, plaintext: &str, label: &str) {
    assert_eq!(
        stored.get("_enc").and_then(|v| v.as_str()),
        Some("v1"),
        "{label}: stored jsonb must be an AES-GCM envelope (_enc=v1), not plaintext: {stored}"
    );
    let nonce = stored
        .get("nonce")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("{label}: envelope missing nonce: {stored}"));
    let ct = stored
        .get("ct")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("{label}: envelope missing ct: {stored}"));
    assert_eq!(
        nonce.len(),
        24,
        "{label}: nonce must be 12-byte hex (24 chars): {nonce}"
    );
    assert!(
        nonce.bytes().all(|b| b.is_ascii_hexdigit()),
        "{label}: nonce must be hex: {nonce}"
    );
    hex::decode(nonce).unwrap_or_else(|e| panic!("{label}: nonce hex decode: {e}: {nonce}"));
    assert!(
        ct.len() >= 32 && ct.len() % 2 == 0 && ct.bytes().all(|b| b.is_ascii_hexdigit()),
        "{label}: ct must be non-empty even hex (ciphertext+tag): {ct}"
    );
    hex::decode(ct).unwrap_or_else(|e| panic!("{label}: ct hex decode: {e}: {ct}"));
    let obj = stored
        .as_object()
        .unwrap_or_else(|| panic!("{label}: envelope must be a JSON object: {stored}"));
    let keys: std::collections::BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["_enc", "nonce", "ct"].into_iter().collect(),
        "{label}: envelope keys must be _enc/nonce/ct only (no access_token / refresh_token / client_secret): {stored}"
    );
    let raw = stored.to_string();
    assert!(
        !raw.contains(plaintext),
        "{label}: raw credentials jsonb/text must not contain the plaintext secret: {raw}"
    );
}

/// Product `GET /api/workspaces/:id/integrations` — catalog + connection
/// public view. Must omit tokens even when the workspace is connected.
async fn list_integrations_http(
    app: &axum::Router,
    access_jwt: &str,
    workspace_id: Uuid,
) -> (StatusCode, String) {
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/workspaces/{workspace_id}/integrations"))
        .header(header::COOKIE, format!("{ACCESS_COOKIE}={access_jwt}"))
        .body(Body::empty())
        .expect("list integrations request");
    let res = app
        .clone()
        .oneshot(req)
        .await
        .expect("list_integrations oneshot");
    let status = res.status();
    let bytes = to_bytes(res.into_body(), 1024 * 1024)
        .await
        .expect("list integrations body");
    let body = String::from_utf8_lossy(&bytes).into_owned();
    (status, body)
}

/// Product `PUT /api/workspaces/:id/integrations/:provider/oauth-app`.
async fn put_oauth_app_http(
    app: &axum::Router,
    access_jwt: &str,
    workspace_id: Uuid,
    provider: &str,
    client_id: &str,
    client_secret: &str,
) -> (StatusCode, String) {
    let req = Request::builder()
        .method("PUT")
        .uri(format!(
            "/api/workspaces/{workspace_id}/integrations/{provider}/oauth-app"
        ))
        .header(header::COOKIE, format!("{ACCESS_COOKIE}={access_jwt}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::json!({
                "client_id": client_id,
                "client_secret": client_secret,
            })
            .to_string(),
        ))
        .expect("put oauth-app request");
    let res = app
        .clone()
        .oneshot(req)
        .await
        .expect("put_oauth_app oneshot");
    let status = res.status();
    let bytes = to_bytes(res.into_body(), 1024 * 1024)
        .await
        .expect("put oauth-app body");
    let body = String::from_utf8_lossy(&bytes).into_owned();
    (status, body)
}

/// Product `GET /api/integrations/oauth/callback/:provider` — the wrapper
/// that sets 401/403 (not `oauth_callback_inner` alone). `cookie` is the
/// raw `Cookie` header, or `None` for a request with no Cookie at all.
async fn oauth_callback_http(
    app: &axum::Router,
    cookie: Option<&str>,
    code: &str,
    state_jwt: &str,
) -> (StatusCode, String) {
    let mut builder = Request::builder().method("GET").uri(format!(
        "/api/integrations/oauth/callback/trello?code={code}&state={state_jwt}"
    ));
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    let req = builder.body(Body::empty()).expect("callback request");
    let res = app
        .clone()
        .oneshot(req)
        .await
        .expect("oauth_callback oneshot");
    let status = res.status();
    let bytes = to_bytes(res.into_body(), 1024 * 1024)
        .await
        .expect("callback body");
    let body = String::from_utf8_lossy(&bytes).into_owned();
    (status, body)
}

fn assert_callback_rejected_without_session(status: StatusCode, body: &str, context: &str) {
    assert!(
        status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN,
        "{context}: must 401 or 403, not 200 popup-close as connected, got {status}: {body}"
    );
    let lower = body.to_ascii_lowercase();
    assert!(
        !lower.contains("ok: true") && !lower.contains("ok:true"),
        "{context}: reject must not popup-close as connected: {body}"
    );
    assert!(
        !lower.contains("trello connected"),
        "{context}: reject must not report provider connected: {body}"
    );
    assert!(
        !body.contains("access_token")
            && !body.contains("refresh_token")
            && !body.contains("client_secret"),
        "{context}: reject body must not contain access_token / refresh_token / client_secret: {body}"
    );
}

/// Session user must equal `state.sub`. State issued for A completed
/// with member B's `fleet_access` must 403 user_mismatch and write no
/// Trello creds on A or B. Membership of `state.workspace_id` is not enough.
#[tokio::test]
async fn oauth_callback_session_user_must_equal_state_sub() {
    let pool = connect_and_migrate().await;
    let (user_a, _email_a, ws_a) = seed_user_with_workspace(&pool, "sub-a").await;
    let (user_b, email_b, ws_b) = seed_user_with_workspace(&pool, "sub-b").await;

    // B is also a member of A. A membership-only check would pass
    // `require_workspace_member` and encrypt B's code onto A (confused-deputy).
    workspaces::add_member(&pool, ws_a, user_b, "member", Utc::now())
        .await
        .expect("add B as member of A");
    assert!(
        workspaces::find_for_user(&pool, ws_a, user_b)
            .await
            .expect("lookup B on A")
            .is_some(),
        "B must be a member of A so 403 is not only 'not a member'"
    );

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let key = state.config.credentials_key.clone();
    let app = fleet_integrations::router().with_state(state.clone());

    let jwt_a = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "trello")
        .expect("issue product oauth state");
    let mismatch_tok = format!("tok_must_not_land_on_b_{}", Uuid::now_v7());
    let b_cookie = session_cookie(&state, user_b, &email_b).await;

    let (rejected, rejected_body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={b_cookie}")),
        &mismatch_tok,
        &jwt_a,
    )
    .await;
    assert_eq!(
        rejected,
        StatusCode::FORBIDDEN,
        "state issued for A completed via member-B cookie must 403, not 200 popup-close, got {rejected}: {rejected_body}"
    );
    assert!(
        rejected_body.to_ascii_lowercase().contains("user mismatch"),
        "user-mismatch must say OAuth state user mismatch, got {rejected_body}"
    );

    assert!(
        creds_on(&pool, ws_b, &key).await.is_none(),
        "must not encrypt tokens onto workspace B"
    );
    assert!(
        creds_on(&pool, ws_a, &key).await.is_none(),
        "rejected user-mismatch must not complete Connect onto A either"
    );
}

/// Product-issued Trello state for A's second workspace must bind there,
/// not to the first workspace A owns.
#[tokio::test]
async fn oauth_callback_same_user_state_for_second_workspace_binds_there() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "bind-a2").await;
    let ws_a2 = Uuid::now_v7();
    workspaces::create_owned(
        &pool,
        ws_a2,
        user_a,
        &format!("owb-a2-{ws_a2}"),
        "OAuth ws bind A2",
        Utc::now(),
    )
    .await
    .expect("create second workspace for A");

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let key = state.config.credentials_key.clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;

    let jwt_a2 = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a2, "trello")
        .expect("issue product oauth state for A2");
    let tok_a2 = format!("tok_ws_a2_{}", Uuid::now_v7());
    let (ok_status, ok_body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &tok_a2,
        &jwt_a2,
    )
    .await;
    assert_eq!(
        ok_status,
        StatusCode::OK,
        "state A2 + session A must complete Connect onto A2, got {ok_status}: {ok_body}"
    );

    let stored_a2 = creds_on(&pool, ws_a2, &key)
        .await
        .expect("tokens must be encrypted onto the state workspace A2");
    assert_eq!(stored_a2["access_token"], tok_a2);
    assert!(
        creds_on(&pool, ws_a, &key).await.is_none(),
        "Connect for A2 must not encrypt tokens onto first-owned workspace A"
    );
}

/// User-mismatch 403 must not consume the state nonce. Retrying the same
/// JWT with A's matching cookie must still complete Connect onto A.
#[tokio::test]
async fn oauth_callback_user_mismatch_does_not_consume_nonce() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "nonce-a").await;
    let (user_b, email_b, _ws_b) = seed_user_with_workspace(&pool, "nonce-b").await;
    workspaces::add_member(&pool, ws_a, user_b, "member", Utc::now())
        .await
        .expect("add B as member of A");

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let key = state.config.credentials_key.clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;
    let b_cookie = session_cookie(&state, user_b, &email_b).await;

    let jwt_a = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "trello")
        .expect("issue product oauth state");
    let mismatch_tok = format!("tok_mismatch_nonce_{}", Uuid::now_v7());
    let (rejected, rejected_body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={b_cookie}")),
        &mismatch_tok,
        &jwt_a,
    )
    .await;
    assert_eq!(
        rejected,
        StatusCode::FORBIDDEN,
        "member-B cookie on A's state must 403, got {rejected}: {rejected_body}"
    );
    assert!(
        rejected_body.to_ascii_lowercase().contains("user mismatch"),
        "403 must be user mismatch so consume-after-bind is the path, got {rejected_body}"
    );
    assert!(
        creds_on(&pool, ws_a, &key).await.is_none(),
        "user-mismatch must not upsert before the matching retry"
    );

    let ok_tok = format!("tok_retry_same_jwt_{}", Uuid::now_v7());
    let (ok_status, ok_body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &ok_tok,
        &jwt_a,
    )
    .await;
    assert_eq!(
        ok_status,
        StatusCode::OK,
        "same JWT + A cookie after user-mismatch must still Connect, got {ok_status}: {ok_body}"
    );
    let stored_a = creds_on(&pool, ws_a, &key)
        .await
        .expect("retry must encrypt tokens onto A");
    assert_eq!(stored_a["access_token"], ok_tok);
    assert_ne!(stored_a["access_token"], mismatch_tok);
}

/// Revoked membership of `state.sub` on `state.workspace_id` is 403
/// not-a-member (not user mismatch) and must not upsert.
#[tokio::test]
async fn oauth_callback_revoked_state_user_membership_is_403_not_a_member() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "rev-a").await;

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let key = state.config.credentials_key.clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;

    let jwt_a = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "trello")
        .expect("issue product oauth state");
    let removed = workspaces::remove_member(&pool, ws_a, user_a)
        .await
        .expect("remove A from ws_a");
    assert!(removed, "A must be removed so callback is not still a member");
    assert!(
        workspaces::find_for_user(&pool, ws_a, user_a)
            .await
            .expect("lookup A on A")
            .is_none(),
        "A must not be a member of ws_a after revoke"
    );

    let tok = format!("tok_revoked_member_{}", Uuid::now_v7());
    let (status, body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &tok,
        &jwt_a,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "revoked membership must 403, not 200 popup-close, got {status}: {body}"
    );
    let lower = body.to_ascii_lowercase();
    assert!(
        lower.contains("not a member"),
        "403 body must name not a member, got {body}"
    );
    assert!(
        !lower.contains("user mismatch"),
        "revoked membership must not be labeled user mismatch: {body}"
    );
    assert!(
        creds_on(&pool, ws_a, &key).await.is_none(),
        "revoked membership must not encrypt tokens onto A"
    );
}

/// Stolen `state` JWT + `code` (logs, Referer, a copied callback URL) must
/// not complete Connect. SameSite=Lax sends `fleet_access` on a real
/// top-level provider redirect; a missing cookie is not a guest Connect.
/// Refresh-only (`fleet_refresh`, no `fleet_access`) is the same missing
/// access cookie. Drive the HTTP wrapper so 200 popup-close-as-connected
/// cannot hide an inner Ok.
#[tokio::test]
async fn oauth_callback_requires_session_cookie() {
    let pool = connect_and_migrate().await;
    let (user_a, _email_a, ws_a) = seed_user_with_workspace(&pool, "sess-a").await;
    let ws_a2 = Uuid::now_v7();
    workspaces::create_owned(
        &pool,
        ws_a2,
        user_a,
        &format!("ows-a2-{ws_a2}"),
        "OAuth session A2",
        Utc::now(),
    )
    .await
    .expect("create second workspace for A");

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let key = state.config.credentials_key.clone();
    let client_secret = state
        .config
        .trello
        .as_ref()
        .expect("trello test app")
        .client_secret
        .clone();
    let app = fleet_integrations::router().with_state(state.clone());

    // Product start: issue a valid Trello state for owner A. Do not
    // pre-insert the nonce — that would make no-cookie fail as reuse.
    let jwt_no_cookie = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "trello")
        .expect("issue product oauth state");
    let stolen = format!("tok_stolen_no_cookie_{}", Uuid::now_v7());
    let (no_cookie_status, no_cookie_body) =
        oauth_callback_http(&app, None, &stolen, &jwt_no_cookie).await;
    assert_eq!(
        no_cookie_status,
        StatusCode::UNAUTHORIZED,
        "no Cookie header must 401, not 200 popup-close as connected, got {no_cookie_status}: {no_cookie_body}"
    );
    assert_callback_rejected_without_session(no_cookie_status, &no_cookie_body, "no Cookie header");
    assert!(
        !no_cookie_body.contains(&stolen) && !no_cookie_body.contains(&client_secret),
        "no-cookie reject must not echo the stolen token or client_secret: {no_cookie_body}"
    );
    assert!(
        creds_on(&pool, ws_a, &key).await.is_none(),
        "no-cookie callback must not encrypt tokens onto A"
    );
    assert!(
        creds_on(&pool, ws_a2, &key).await.is_none(),
        "no-cookie callback must not encrypt tokens onto A's other workspace"
    );

    // Refresh-only cookie is not a session. Same wrapper is_none() 401.
    let jwt_refresh = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "trello")
        .expect("issue refresh-only probe state");
    let refresh_tok = format!("tok_refresh_only_{}", Uuid::now_v7());
    let refresh_only = format!("{REFRESH_COOKIE}={}", Uuid::now_v7().as_simple());
    assert!(
        !refresh_only.contains(ACCESS_COOKIE),
        "refresh-only header must not include {ACCESS_COOKIE}"
    );
    let (refresh_status, refresh_body) =
        oauth_callback_http(&app, Some(&refresh_only), &refresh_tok, &jwt_refresh).await;
    assert_eq!(
        refresh_status,
        StatusCode::UNAUTHORIZED,
        "fleet_refresh without fleet_access must 401, got {refresh_status}: {refresh_body}"
    );
    assert_callback_rejected_without_session(
        refresh_status,
        &refresh_body,
        "fleet_refresh without fleet_access",
    );
    assert!(
        !refresh_body.contains(&refresh_tok) && !refresh_body.contains(&client_secret),
        "refresh-only reject must not echo the token or client_secret: {refresh_body}"
    );
    assert!(
        creds_on(&pool, ws_a, &key).await.is_none(),
        "refresh-only cookie must not upsert credentials onto A"
    );
    assert!(
        creds_on(&pool, ws_a2, &key).await.is_none(),
        "refresh-only cookie must not upsert onto A's other workspace"
    );
}

/// Wrapper rejects missing `fleet_access` before `consume_oauth_state_nonce`.
/// After a no-cookie 401 of a product-issued Trello JWT, matching
/// `fleet_access` retry of THAT same JWT must 200 and store the new code.
#[tokio::test]
async fn oauth_callback_no_cookie_does_not_consume_nonce() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "sess-nonce").await;

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let key = state.config.credentials_key.clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;

    let jwt = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "trello")
        .expect("issue product oauth state");
    let stolen = format!("tok_stolen_nonce_{}", Uuid::now_v7());
    let (rejected, rejected_body) = oauth_callback_http(&app, None, &stolen, &jwt).await;
    assert_eq!(
        rejected,
        StatusCode::UNAUTHORIZED,
        "no-cookie must 401 before consume, got {rejected}: {rejected_body}"
    );
    assert!(
        creds_on(&pool, ws_a, &key).await.is_none(),
        "no-cookie reject must not upsert before the matching retry"
    );

    let ok_tok = format!("tok_retry_same_jwt_{}", Uuid::now_v7());
    let (ok_status, ok_body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &ok_tok,
        &jwt,
    )
    .await;
    assert_eq!(
        ok_status,
        StatusCode::OK,
        "same JWT + A cookie after no-cookie must still Connect, got {ok_status}: {ok_body}"
    );
    let stored_a = creds_on(&pool, ws_a, &key)
        .await
        .expect("retry must encrypt tokens onto A");
    assert_eq!(stored_a["access_token"], ok_tok);
    assert_ne!(stored_a["access_token"], stolen);
}

/// Present-but-invalid `fleet_access` is not a session. The wrapper only
/// checks `jar.get(ACCESS_COOKIE).is_none()`, so `not-a-jwt` and an expired
/// access JWT still reach `require_user` and must 401 without upsert.
#[tokio::test]
async fn oauth_callback_invalid_access_cookie_must_not_complete_connect() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "sess-bad").await;
    let ws_a2 = Uuid::now_v7();
    workspaces::create_owned(
        &pool,
        ws_a2,
        user_a,
        &format!("ows-bad-a2-{ws_a2}"),
        "OAuth invalid-cookie A2",
        Utc::now(),
    )
    .await
    .expect("create second workspace for A");

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let key = state.config.credentials_key.clone();
    let app = fleet_integrations::router().with_state(state.clone());

    let jwt_garbage = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "trello")
        .expect("issue product oauth state");
    let garbage_tok = format!("tok_not_a_jwt_{}", Uuid::now_v7());
    let (garbage_status, garbage_body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}=not-a-jwt")),
        &garbage_tok,
        &jwt_garbage,
    )
    .await;
    assert_eq!(
        garbage_status,
        StatusCode::UNAUTHORIZED,
        "fleet_access=not-a-jwt must 401, got {garbage_status}: {garbage_body}"
    );
    assert_callback_rejected_without_session(
        garbage_status,
        &garbage_body,
        "fleet_access=not-a-jwt",
    );
    assert!(
        creds_on(&pool, ws_a, &key).await.is_none()
            && creds_on(&pool, ws_a2, &key).await.is_none(),
        "not-a-jwt access cookie must not upsert onto A or A2"
    );

    // Live session + expired access JWT: if the handler skipped exp, this
    // would complete Connect. jsonwebtoken default leeway is 60s.
    let session_id = Uuid::now_v7();
    let now = Utc::now();
    session_repo::insert(
        &state.db,
        &NewAuthSession {
            id: session_id,
            user_id: user_a,
            refresh_token_hash: session_id.as_bytes().to_vec(),
            family_id: Uuid::now_v7(),
            user_agent: Some("oauth-ws-bind-test".into()),
            ip: None,
            expires_at: now + chrono::Duration::hours(1),
            absolute_expires_at: now + chrono::Duration::days(1),
        },
    )
    .await
    .expect("insert session");
    #[derive(Serialize)]
    struct AccessClaims {
        sub: String,
        email: String,
        sid: String,
        exp: i64,
        iat: i64,
    }
    let ts = now.timestamp();
    let expired_access = encode(
        &Header::default(),
        &AccessClaims {
            sub: user_a.to_string(),
            email: email_a.to_string(),
            sid: session_id.to_string(),
            exp: ts - 180,
            iat: ts - 240,
        },
        &EncodingKey::from_secret(state.auth.jwt_secret.as_bytes()),
    )
    .expect("expired access jwt");

    let jwt_expired = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "trello")
        .expect("issue expired-cookie probe state");
    let expired_tok = format!("tok_expired_access_{}", Uuid::now_v7());
    let (expired_status, expired_body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={expired_access}")),
        &expired_tok,
        &jwt_expired,
    )
    .await;
    assert_eq!(
        expired_status,
        StatusCode::UNAUTHORIZED,
        "expired fleet_access must 401, got {expired_status}: {expired_body}"
    );
    assert_callback_rejected_without_session(
        expired_status,
        &expired_body,
        "expired fleet_access JWT",
    );
    assert!(
        creds_on(&pool, ws_a, &key).await.is_none(),
        "expired access cookie must not upsert onto A"
    );
    assert!(
        creds_on(&pool, ws_a2, &key).await.is_none(),
        "expired access cookie must not upsert onto A's other workspace"
    );
}

/// `Cookie: fleet_access=` is `Some("")` so the wrapper presence check
/// passes; bind/`require_user` must still 401 and not upsert.
#[tokio::test]
async fn oauth_callback_empty_access_cookie_value_must_not_complete_connect() {
    let pool = connect_and_migrate().await;
    let (user_a, _email_a, ws_a) = seed_user_with_workspace(&pool, "sess-empty").await;
    let ws_a2 = Uuid::now_v7();
    workspaces::create_owned(
        &pool,
        ws_a2,
        user_a,
        &format!("ows-empty-a2-{ws_a2}"),
        "OAuth empty-cookie A2",
        Utc::now(),
    )
    .await
    .expect("create second workspace for A");

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let key = state.config.credentials_key.clone();
    let app = fleet_integrations::router().with_state(state.clone());

    let jwt = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "trello")
        .expect("issue product oauth state");
    let stolen = format!("tok_empty_access_{}", Uuid::now_v7());
    let (status, body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}=")),
        &stolen,
        &jwt,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "empty fleet_access value must 401, got {status}: {body}"
    );
    assert_callback_rejected_without_session(status, &body, "empty fleet_access value");
    assert!(
        creds_on(&pool, ws_a, &key).await.is_none(),
        "empty access cookie must not upsert onto A"
    );
    assert!(
        creds_on(&pool, ws_a2, &key).await.is_none(),
        "empty access cookie must not upsert onto A's other workspace"
    );
}

/// Expired OAuth `state` JWT must not complete Connect. `verify_oauth_state`
/// is a unit only — this drives `oauth_callback` so a handler that skipped
/// verify, set `validate_exp = false`, or upserted before verify cannot pass.
/// Nonce is not consumed on verify failure.
#[tokio::test]
async fn oauth_expired_state_must_not_complete_connect() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "exp-a").await;
    let ws_a2 = Uuid::now_v7();
    workspaces::create_owned(
        &pool,
        ws_a2,
        user_a,
        &format!("owe-a2-{ws_a2}"),
        "OAuth expired A2",
        Utc::now(),
    )
    .await
    .expect("create second workspace for A");

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let key = state.config.credentials_key.clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;

    let nonce = Uuid::now_v7().to_string();
    let now = Utc::now().timestamp();
    // iat-60 with iat=now-180 → exp=now-240, past jsonwebtoken's 60s leeway.
    let expired_jwt = encode_oauth_state(
        &secret,
        user_a,
        ws_a,
        "trello",
        &nonce,
        now - 180,
        -60,
        "oauth_state",
    );

    let expired_tok = format!("tok_expired_state_{}", Uuid::now_v7());
    let (expired_status, expired_body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &expired_tok,
        &expired_jwt,
    )
    .await;
    assert_eq!(
        expired_status,
        StatusCode::UNAUTHORIZED,
        "expired OAuth state must 401, not 200 popup-close as connected, got {expired_status}: {expired_body}"
    );
    assert!(
        creds_on(&pool, ws_a, &key).await.is_none(),
        "expired OAuth state must not encrypt tokens onto A"
    );
    assert!(
        creds_on(&pool, ws_a2, &key).await.is_none(),
        "expired OAuth state must not encrypt tokens onto A's other workspace"
    );

    let jwt_ok = encode_oauth_state(
        &secret,
        user_a,
        ws_a,
        "trello",
        &nonce,
        now,
        600,
        "oauth_state",
    );
    let ok_tok = format!("tok_exp_ok_{}", Uuid::now_v7());
    let (ok_status, ok_body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &ok_tok,
        &jwt_ok,
    )
    .await;
    assert_eq!(
        ok_status,
        StatusCode::OK,
        "same-nonce unexpired state after expired attempt must still Connect, got {ok_status}: {ok_body}"
    );
    let stored_a = creds_on(&pool, ws_a, &key)
        .await
        .expect("tokens must be encrypted onto the state workspace A");
    assert_eq!(stored_a["access_token"], ok_tok);
    assert_ne!(stored_a["access_token"], expired_tok);
    assert!(
        creds_on(&pool, ws_a2, &key).await.is_none(),
        "successful Connect for A must not encrypt tokens onto A's other workspace"
    );
}

/// `github_manifest_callback_inner` maps `verify_oauth_state` independently
/// of user-oauth. Expired `typ=oauth_state` must 401 and not persist an app.
#[tokio::test]
async fn github_manifest_callback_expired_state_must_401_and_not_persist() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "gh-exp").await;

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;

    let nonce = Uuid::now_v7().to_string();
    let now = Utc::now().timestamp();
    let expired_jwt = encode_oauth_state(
        &secret,
        user_a,
        ws_a,
        "github",
        &nonce,
        now - 180,
        -60,
        "oauth_state",
    );
    let code = format!("manifest-code-expired-{}", Uuid::now_v7());
    let (status, body) = github_manifest_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &code,
        &expired_jwt,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "expired manifest state must 401, got {status}: {body}"
    );
    assert!(
        github_oauth_apps(&pool, ws_a).await.is_empty(),
        "expired manifest state must not persist an oauth app onto A"
    );
}

/// Well-signed `typ=access` (future exp, UUID nonce) is not `oauth_state`.
/// A skipped typ arm would complete Trello Connect.
#[tokio::test]
async fn oauth_callback_access_typ_state_must_not_complete_connect() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "typ-a").await;

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let key = state.config.credentials_key.clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;

    let nonce = Uuid::now_v7().to_string();
    let now = Utc::now().timestamp();
    let access_jwt = encode_oauth_state(
        &secret,
        user_a,
        ws_a,
        "trello",
        &nonce,
        now,
        600,
        "access",
    );
    let tok = format!("tok_access_typ_{}", Uuid::now_v7());
    let (status, body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &tok,
        &access_jwt,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "typ=access state must 401, not 200 popup-close as connected, got {status}: {body}"
    );
    assert!(
        creds_on(&pool, ws_a, &key).await.is_none(),
        "typ=access state must not encrypt tokens onto A"
    );
}

/// JWT signed with a second secret must not complete Connect. HMAC fail
/// must not consume the nonce — a well-signed same-nonce retry still Connects.
#[tokio::test]
async fn oauth_callback_wrong_secret_state_must_not_complete_connect() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "hmac-a").await;

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let key = state.config.credentials_key.clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;

    let nonce = Uuid::now_v7().to_string();
    let now = Utc::now().timestamp();
    let forged = encode_oauth_state(
        "oauth-ws-bind-other-secret-32chars!",
        user_a,
        ws_a,
        "trello",
        &nonce,
        now,
        600,
        "oauth_state",
    );
    let forged_tok = format!("tok_wrong_secret_{}", Uuid::now_v7());
    let (rejected, rejected_body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &forged_tok,
        &forged,
    )
    .await;
    assert_eq!(
        rejected,
        StatusCode::UNAUTHORIZED,
        "wrong-secret state must 401, got {rejected}: {rejected_body}"
    );
    assert!(
        creds_on(&pool, ws_a, &key).await.is_none(),
        "wrong-secret state must not encrypt tokens onto A"
    );

    let issued = encode_oauth_state(
        &secret,
        user_a,
        ws_a,
        "trello",
        &nonce,
        now,
        600,
        "oauth_state",
    );
    let ok_tok = format!("tok_hmac_ok_{}", Uuid::now_v7());
    let (ok_status, ok_body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &ok_tok,
        &issued,
    )
    .await;
    assert_eq!(
        ok_status,
        StatusCode::OK,
        "issued same-nonce after wrong-secret reject must still Connect, got {ok_status}: {ok_body}"
    );
    let stored_a = creds_on(&pool, ws_a, &key)
        .await
        .expect("same-nonce retry must encrypt tokens onto A");
    assert_eq!(stored_a["access_token"], ok_tok);
    assert_ne!(stored_a["access_token"], forged_tok);
}

/// After a real Trello Connect, `integration_connections.credentials` must
/// be an AES-GCM envelope (`_enc=v1`, nonce+ct hex). `credentials_for`
/// decrypts *and* passes through a raw `{access_token}` blob, so a skipped
/// `encrypt()` before upsert still greens the bind / reuse / session cards.
/// One SQL envelope + decrypt + callback HTML omit (including `_enc`/`ct`
/// so leaking the sealed blob fails).
#[tokio::test]
async fn oauth_callback_credentials_encrypted_at_rest() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "enc-a").await;

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let key = state.config.credentials_key.clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;

    let jwt_ok = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "trello")
        .expect("issue control oauth state");
    let ok_tok = format!("tok_enc_at_rest_{}", Uuid::now_v7());
    let (ok_status, ok_body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &ok_tok,
        &jwt_ok,
    )
    .await;
    assert_eq!(
        ok_status,
        StatusCode::OK,
        "A cookie + unexpired A Trello state must complete Connect, got {ok_status}: {ok_body}"
    );
    let ok_lower = ok_body.to_ascii_lowercase();
    assert!(
        ok_lower.contains("ok: true") || ok_lower.contains("ok:true"),
        "callback HTML must popup-close as connected so omit-token is not vacuous: {ok_body}"
    );
    assert!(
        !ok_body.contains(&ok_tok)
            && !ok_body.contains("access_token")
            && !ok_body.contains("\"_enc\"")
            && !ok_body.contains("\"nonce\"")
            && !ok_body.contains("\"ct\""),
        "callback HTML must omit the planted token and sealed envelope fields: {ok_body}"
    );

    let decrypted = creds_on(&pool, ws_a, &key)
        .await
        .expect("credentials_for(A, trello) must be Some after Connect");
    assert_eq!(
        decrypted["access_token"], ok_tok,
        "credentials_for must decrypt to the planted access_token: {decrypted}"
    );

    let raw_text = credentials_text(&pool, ws_a, "trello").await;
    assert!(
        !raw_text.contains(&ok_tok),
        "raw credentials::text must not contain the plaintext token: {raw_text}"
    );
    let raw_json: serde_json::Value =
        serde_json::from_str(&raw_text).expect("credentials::text must be JSON");
    assert_aes_gcm_envelope(&raw_json, &ok_tok, "credentials::text");
}

/// PUT workspace GitHub oauth-app encrypts `client_secret` before
/// `upsert_oauth_app`. Unit serialize of PutOauthAppResponse never hits
/// persist_oauth_app.
#[tokio::test]
async fn put_oauth_app_client_secret_encrypted_at_rest() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "enc-app").await;

    let state = test_state(pool.clone());
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;

    let client_id = format!("gh_oauth_app_id_{}", Uuid::now_v7());
    let client_secret = format!("gh_oauth_app_secret_{}", Uuid::now_v7());
    let (status, body) = put_oauth_app_http(
        &app,
        &a_cookie,
        ws_a,
        "github",
        &client_id,
        &client_secret,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "PUT github oauth-app must 200, got {status}: {body}"
    );

    let raw_text: String = sqlx::query_scalar(
        r#"
        SELECT client_secret::text
        FROM integration_oauth_apps
        WHERE workspace_id = $1 AND provider = $2
        "#,
    )
    .bind(ws_a)
    .bind("github")
    .fetch_one(&pool)
    .await
    .expect("SELECT integration_oauth_apps.client_secret::text");
    assert!(
        !raw_text.contains(&client_secret),
        "client_secret::text must not contain the planted secret: {raw_text}"
    );
    let raw_json: serde_json::Value =
        serde_json::from_str(&raw_text).expect("client_secret::text must be JSON");
    assert_aes_gcm_envelope(
        &raw_json,
        &client_secret,
        "integration_oauth_apps.client_secret",
    );
}

/// Legacy plaintext `{access_token}` rows stay readable until reconnect;
/// a matching Trello callback must rewrite the jsonb as a v1 envelope
/// that contains neither the old nor the new token.
#[tokio::test]
async fn reconnect_rewrites_legacy_plaintext_credentials_as_envelope() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "enc-legacy").await;

    let tok_legacy_plain = format!("tok_legacy_plain_{}", Uuid::now_v7());
    integ_repo::upsert(
        &pool,
        &integ_repo::UpsertConnection {
            id: Uuid::now_v7(),
            workspace_id: ws_a,
            provider: "trello".into(),
            account_label: Some("legacy".into()),
            config: serde_json::json!({}),
            credentials: serde_json::json!({ "access_token": tok_legacy_plain }),
            connected_by: Some(user_a),
            at: Utc::now(),
        },
    )
    .await
    .expect("plant legacy plaintext credentials");
    let planted = credentials_text(&pool, ws_a, "trello").await;
    assert!(
        planted.contains(&tok_legacy_plain) && !planted.contains("\"_enc\""),
        "setup must start from a plaintext credentials row: {planted}"
    );

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let key = state.config.credentials_key.clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;

    let jwt = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "trello")
        .expect("issue reconnect oauth state");
    let tok_new = format!("tok_new_{}", Uuid::now_v7());
    let (ok_status, ok_body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &tok_new,
        &jwt,
    )
    .await;
    assert_eq!(
        ok_status,
        StatusCode::OK,
        "reconnect callback must complete Connect, got {ok_status}: {ok_body}"
    );

    let decrypted = creds_on(&pool, ws_a, &key)
        .await
        .expect("reconnect must leave decrypted credentials");
    assert_eq!(decrypted["access_token"], tok_new);
    assert_ne!(decrypted["access_token"], tok_legacy_plain);

    let raw_text = credentials_text(&pool, ws_a, "trello").await;
    assert!(
        !raw_text.contains(&tok_legacy_plain) && !raw_text.contains(&tok_new),
        "credentials::text must contain neither tok_legacy_plain nor tok_new: {raw_text}"
    );
    let raw_json: serde_json::Value =
        serde_json::from_str(&raw_text).expect("credentials::text must be JSON");
    assert_aes_gcm_envelope(&raw_json, &tok_new, "reconnect credentials::text");
}

/// GET /integrations after Connect must omit sealed envelope keys.
/// Unit serialize of IntegrationConnectionPublic never hits the list handler.
#[tokio::test]
async fn list_integrations_after_connect_omits_envelope_fields() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "enc-list").await;

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;

    let jwt = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "trello")
        .expect("issue list-omit oauth state");
    let ok_tok = format!("tok_enc_list_{}", Uuid::now_v7());
    let (ok_status, ok_body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &ok_tok,
        &jwt,
    )
    .await;
    assert_eq!(
        ok_status,
        StatusCode::OK,
        "Connect setup must succeed, got {ok_status}: {ok_body}"
    );

    let (list_status, list_body) = list_integrations_http(&app, &a_cookie, ws_a).await;
    assert_eq!(
        list_status,
        StatusCode::OK,
        "GET /api/workspaces/{{A}}/integrations after Connect: {list_status}: {list_body}"
    );
    let list_json: serde_json::Value =
        serde_json::from_str(&list_body).expect("GET /integrations JSON");
    let trello = list_json["integrations"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|i| i["id"] == "trello")
        .unwrap_or_else(|| panic!("trello missing from catalog: {list_body}"));
    assert_eq!(
        trello["connected"], true,
        "trello must show connected so omit-envelope is not vacuous: {list_body}"
    );
    assert!(
        trello.get("credentials").is_none()
            && trello.get("_enc").is_none()
            && trello.get("nonce").is_none()
            && trello.get("ct").is_none(),
        "trello catalog view must omit envelope keys: {trello}"
    );
    let conn = trello
        .get("connection")
        .unwrap_or_else(|| panic!("connected trello must include connection view: {trello}"));
    assert!(
        conn.get("credentials").is_none()
            && conn.get("_enc").is_none()
            && conn.get("nonce").is_none()
            && conn.get("ct").is_none(),
        "connection public view must omit envelope keys: {conn}"
    );
    assert!(
        !list_body.contains(&ok_tok)
            && !list_body.contains("\"credentials\"")
            && !list_body.contains("\"_enc\"")
            && !list_body.contains("\"nonce\"")
            && !list_body.contains("\"ct\""),
        "GET /integrations must omit the planted token and envelope keys: {list_body}"
    );
}

/// Provider mismatch is a typed 400, not 200 popup-close. GitHub state on
/// the Trello callback must not consume the nonce or encrypt the attacker token.
#[tokio::test]
async fn oauth_callback_provider_mismatch_is_400() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "pm-a").await;
    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let key = state.config.credentials_key.clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;

    let jwt_github = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "github")
        .expect("issue github state");
    let claims = fleet_integrations::verify_oauth_state(&secret, &jwt_github)
        .expect("decode issued github state");
    let attacker = format!("tok_provider_mismatch_{}", Uuid::now_v7());
    let (status, body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &attacker,
        &jwt_github,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "provider mismatch must 400, not 200 popup-close, got {status}: {body}"
    );
    let lower = body.to_ascii_lowercase();
    assert!(
        !lower.contains("ok: true") && !lower.contains("ok:true"),
        "provider mismatch must not popup-close as connected: {body}"
    );
    assert!(
        creds_on(&pool, ws_a, &key).await.is_none(),
        "provider mismatch must not encrypt tokens onto A"
    );
    let nonce_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM integration_oauth_state_nonces WHERE nonce = $1",
    )
    .bind(&claims.nonce)
    .fetch_one(&pool)
    .await
    .expect("count oauth nonce");
    assert_eq!(
        nonce_rows, 0,
        "provider mismatch must not insert a nonce row"
    );
}

/// After github-on-trello 400, `consume_oauth_state_nonce` must still win.
#[tokio::test]
async fn oauth_callback_provider_mismatch_does_not_consume_nonce() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "pm-nc").await;
    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;

    let jwt_github = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "github")
        .expect("issue github state");
    let claims = fleet_integrations::verify_oauth_state(&secret, &jwt_github)
        .expect("decode issued github state");
    let attacker = format!("tok_pm_nonce_{}", Uuid::now_v7());
    let (status, body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &attacker,
        &jwt_github,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "provider mismatch must 400 before consume, got {status}: {body}"
    );

    let still_github =
        integ_repo::consume_oauth_state_nonce(&pool, &claims.nonce, ws_a, "github")
            .await
            .expect("consume github after mismatch");
    assert!(
        still_github,
        "consume_oauth_state_nonce(github) must still win after provider mismatch"
    );
}

/// GitHub state on the Trello callback must not upsert provider=github.
#[tokio::test]
async fn oauth_callback_mismatch_does_not_encrypt_as_state_provider() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "pm-gh").await;
    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let key = state.config.credentials_key.clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;

    let jwt_github = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "github")
        .expect("issue github state");
    let attacker = format!("tok_pm_as_github_{}", Uuid::now_v7());
    let (status, body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &attacker,
        &jwt_github,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "provider mismatch must 400, not persist-as-github then 200, got {status}: {body}"
    );
    assert!(
        fleet_integrations::credentials_for(&pool, ws_a, "github", &key)
            .await
            .expect("load github A")
            .is_none(),
        "github-on-trello must not encrypt tokens as provider github"
    );
}

/// First Connect consumes the nonce; replay is typed 409 on the HTTP wrapper.
#[tokio::test]
async fn oauth_callback_nonce_reuse_is_409() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "nr-a").await;
    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let key = state.config.credentials_key.clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;

    let jwt = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "trello")
        .expect("issue trello state");
    let first_tok = format!("tok_nr_first_{}", Uuid::now_v7());
    let (ok_status, ok_body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &first_tok,
        &jwt,
    )
    .await;
    assert_eq!(
        ok_status,
        StatusCode::OK,
        "first Connect must succeed, got {ok_status}: {ok_body}"
    );

    let replay_tok = format!("tok_nr_replay_{}", Uuid::now_v7());
    let (replay_status, replay_body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &replay_tok,
        &jwt,
    )
    .await;
    assert_eq!(
        replay_status,
        StatusCode::CONFLICT,
        "nonce reuse must 409, not 200 popup-close, got {replay_status}: {replay_body}"
    );
    let replay_lower = replay_body.to_ascii_lowercase();
    assert!(
        replay_lower.contains("used") || replay_lower.contains("nonce"),
        "409 body must name nonce reuse: {replay_body}"
    );
    let stored = creds_on(&pool, ws_a, &key)
        .await
        .expect("first token must remain");
    assert_eq!(stored["access_token"], first_tok);
    assert_ne!(stored["access_token"], replay_tok);
}

async fn github_manifest_query(
    app: &axum::Router,
    cookie: Option<&str>,
    query: &str,
) -> (StatusCode, String) {
    let mut builder = Request::builder().method("GET").uri(format!(
        "/api/integrations/oauth/github/manifest/callback?{query}"
    ));
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    let req = builder.body(Body::empty()).expect("manifest request");
    let res = app
        .clone()
        .oneshot(req)
        .await
        .expect("github_manifest_callback oneshot");
    let status = res.status();
    let bytes = to_bytes(res.into_body(), 1024 * 1024)
        .await
        .expect("manifest body");
    let body = String::from_utf8_lossy(&bytes).into_owned();
    (status, body)
}

async fn github_manifest_http(
    app: &axum::Router,
    cookie: Option<&str>,
    code: &str,
    state_jwt: &str,
) -> (StatusCode, String) {
    github_manifest_query(app, cookie, &format!("code={code}&state={state_jwt}")).await
}

async fn github_oauth_apps(
    pool: &fleet_db::PgPool,
    workspace_id: Uuid,
) -> Vec<fleet_db::IntegrationOAuthApp> {
    integ_repo::list_oauth_apps(pool, workspace_id)
        .await
        .expect("list_oauth_apps")
}

/// Trello-issued state on GET github/manifest/callback is a typed 400
/// and must not persist an OAuth app.
#[tokio::test]
async fn github_manifest_callback_non_github_state_is_400() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "pm-mf").await;

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;

    let jwt_trello = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "trello")
        .expect("issue trello state");
    let code = format!("manifest-code-pm-{}", Uuid::now_v7());
    let (status, body) = github_manifest_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &code,
        &jwt_trello,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "non-github state on manifest callback must 400, got {status}: {body}"
    );
    let lower = body.to_ascii_lowercase();
    assert!(
        !lower.contains("ok: true") && !lower.contains("ok:true"),
        "non-github manifest state must not popup-close as connected: {body}"
    );
    assert!(
        github_oauth_apps(&pool, ws_a).await.is_empty(),
        "trello state on github manifest must not persist an oauth app"
    );
}

/// Stolen GitHub App manifest `code`+`state` with no Cookie must 401
/// and must not persist. Manifest has no early ACCESS_COOKIE gate;
/// bind_callback_session is what rejects.
#[tokio::test]
async fn github_manifest_callback_no_cookie_must_401() {
    let pool = connect_and_migrate().await;
    let (user_a, _email_a, ws_a) = seed_user_with_workspace(&pool, "gh-nc").await;

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let app = fleet_integrations::router().with_state(state.clone());

    let jwt_a = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "github")
        .expect("issue github manifest state");
    let stolen = format!("manifest-code-stolen-{}", Uuid::now_v7());
    let (status, body) = github_manifest_http(&app, None, &stolen, &jwt_a).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "no-cookie manifest callback must 401, got {status}: {body}"
    );
    assert!(
        github_oauth_apps(&pool, ws_a).await.is_empty(),
        "no-cookie manifest must not persist an oauth app onto A"
    );
}

/// `fleet_refresh` without `fleet_access` is not a session.
#[tokio::test]
async fn github_manifest_callback_refresh_only_must_401() {
    let pool = connect_and_migrate().await;
    let (user_a, _email_a, ws_a) = seed_user_with_workspace(&pool, "gh-rf").await;

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let app = fleet_integrations::router().with_state(state.clone());

    let jwt_a = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "github")
        .expect("issue github manifest state");
    let stolen = format!("manifest-code-refresh-{}", Uuid::now_v7());
    let refresh_only = format!("{REFRESH_COOKIE}={}", Uuid::now_v7().as_simple());
    let (status, body) = github_manifest_http(&app, Some(&refresh_only), &stolen, &jwt_a).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "refresh-only manifest callback must 401, got {status}: {body}"
    );
    assert!(
        github_oauth_apps(&pool, ws_a).await.is_empty(),
        "refresh-only manifest must not persist an oauth app onto A"
    );
}

/// Stolen GitHub App manifest `code`+`state` must not persist when the
/// session user is not `state.sub`. Membership of the state workspace
/// is not enough.
#[tokio::test]
async fn github_manifest_callback_requires_session_user_match() {
    let pool = connect_and_migrate().await;
    let (user_a, _email_a, ws_a) = seed_user_with_workspace(&pool, "gh-a").await;
    let (user_b, email_b, ws_b) = seed_user_with_workspace(&pool, "gh-b").await;
    workspaces::add_member(&pool, ws_a, user_b, "member", Utc::now())
        .await
        .expect("add B as member of A");

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let b_cookie = session_cookie(&state, user_b, &email_b).await;

    let jwt_a = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "github")
        .expect("issue github manifest state");
    let stolen = format!("manifest-code-stolen-{}", Uuid::now_v7());
    let (status, body) = github_manifest_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={b_cookie}")),
        &stolen,
        &jwt_a,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "other-user cookie on A's manifest callback must 403, got {status}: {body}"
    );
    assert!(
        github_oauth_apps(&pool, ws_a).await.is_empty()
            && github_oauth_apps(&pool, ws_b).await.is_empty(),
        "other-user cookie must not persist an oauth app onto A or B"
    );
}

/// Matching session + missing code never reaches convert. Manifest is
/// verify-only: the nonce must still be consumable and no row inserted.
#[tokio::test]
async fn github_manifest_callback_does_not_consume_state_nonce() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "gh-nn").await;

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;

    let jwt_a = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "github")
        .expect("issue github manifest state");
    let claims = fleet_integrations::verify_oauth_state(&secret, &jwt_a)
        .expect("decode issued github state");
    let (status, body) = github_manifest_query(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &format!("state={jwt_a}"),
    )
    .await;
    assert_ne!(
        status,
        StatusCode::UNAUTHORIZED,
        "matching cookie + missing code must not 401: {status}: {body}"
    );
    assert_ne!(
        status,
        StatusCode::FORBIDDEN,
        "matching cookie + missing code must not 403: {status}: {body}"
    );
    assert!(
        status != StatusCode::FOUND && status != StatusCode::TEMPORARY_REDIRECT,
        "missing code must not redirect to install: {status}: {body}"
    );
    assert!(
        github_oauth_apps(&pool, ws_a).await.is_empty(),
        "missing code must not persist an oauth app"
    );

    let nonce_rows: i64 = sqlx::query_scalar(
        "SELECT COUNT(*)::bigint FROM integration_oauth_state_nonces WHERE nonce = $1",
    )
    .bind(&claims.nonce)
    .fetch_one(&pool)
    .await
    .expect("count oauth nonce");
    assert_eq!(
        nonce_rows, 0,
        "manifest verify-only must not insert a nonce row"
    );
    let still = integ_repo::consume_oauth_state_nonce(&pool, &claims.nonce, ws_a, "github")
        .await
        .expect("consume after manifest");
    assert!(
        still,
        "consume_oauth_state_nonce must still win after missing-code manifest"
    );
}

/// GitHub `?error=access_denied` on the matching session must not
/// redirect or persist. Error is checked before convert, so
/// api.github.com is never POSTed.
#[tokio::test]
async fn github_manifest_callback_github_denied_does_not_persist() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "gh-den").await;

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;

    let jwt_a = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "github")
        .expect("issue github manifest state");
    let (status, body) = github_manifest_query(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &format!("error=access_denied&state={jwt_a}"),
    )
    .await;
    assert_ne!(
        status,
        StatusCode::UNAUTHORIZED,
        "matching cookie + GitHub denied must not 401: {status}: {body}"
    );
    assert_ne!(
        status,
        StatusCode::FORBIDDEN,
        "matching cookie + GitHub denied must not 403: {status}: {body}"
    );
    assert!(
        status != StatusCode::FOUND && status != StatusCode::TEMPORARY_REDIRECT,
        "GitHub denied must not redirect to install: {status}: {body}"
    );
    assert!(
        github_oauth_apps(&pool, ws_a).await.is_empty(),
        "GitHub denied must not persist an oauth app"
    );
}
