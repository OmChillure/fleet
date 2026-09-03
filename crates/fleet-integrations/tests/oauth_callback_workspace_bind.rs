//! OAuth callback `state.workspace_id` must bind to that workspace only.
//! A callback with no `fleet_access` session must not complete Connect.
//! An expired OAuth `state` JWT must not complete Connect.
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
    CredentialsKey, IntegrationsConfig, IntegrationsState, OAuthClientConfig, verify_oauth_state,
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
        },
    }
}

fn issue_oauth_state(secret: &str, user_id: Uuid, workspace_id: Uuid, provider: &str) -> String {
    encode_oauth_state(
        secret,
        user_id,
        workspace_id,
        provider,
        &Uuid::now_v7().to_string(),
        Utc::now().timestamp(),
        600,
    )
}

/// Signed `oauth_state` JWT with an explicit nonce and `iat`/`exp` offset.
/// `exp_offset_secs` is added to `iat` (negative → already expired).
fn encode_oauth_state(
    secret: &str,
    user_id: Uuid,
    workspace_id: Uuid,
    provider: &str,
    nonce: &str,
    iat: i64,
    exp_offset_secs: i64,
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
            typ: "oauth_state".into(),
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

/// Stored jsonb via `find_connected` — not `credentials_for` (that decrypts
/// envelopes *and* passes through a raw `{access_token}` blob).
async fn stored_connection(
    pool: &fleet_db::PgPool,
    workspace_id: Uuid,
) -> Option<fleet_db::IntegrationConnection> {
    integ_repo::find_connected(pool, workspace_id, "trello")
        .await
        .expect("find_connected")
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

/// Product `GET /api/integrations/oauth/callback/:provider` — the wrapper
/// that sets `StatusCode::FORBIDDEN` (not `oauth_callback_inner` alone).
async fn oauth_callback(
    app: &axum::Router,
    access_jwt: &str,
    code: &str,
    state_jwt: &str,
) -> StatusCode {
    oauth_callback_http(
        app,
        Some(&format!("{ACCESS_COOKIE}={access_jwt}")),
        code,
        state_jwt,
    )
    .await
    .0
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

/// State issued for workspace A completed with user B's session cookie
/// must 403 even when B is a member of A, and must not encrypt tokens
/// onto B, A, or A's other workspace. Matching A session binds to A only.
/// Both sides stay trello (provider mismatch is a separate card).
#[tokio::test]
async fn oauth_callback_state_workspace_must_match_session_workspace() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "a").await;
    let (user_b, email_b, ws_b) = seed_user_with_workspace(&pool, "b").await;
    // Same user also owns a second workspace so ignoring state.workspace_id
    // would have a plausible place to encrypt the token.
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

    // B is also a member of A. A membership-only check would pass
    // `require_workspace_member` and encrypt B's code onto A (confused-deputy).
    // Callback must still 403: session user must equal state.sub.
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

    let jwt_a = issue_oauth_state(&secret, user_a, ws_a, "trello");
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
        "state issued for ws A completed via ws B cookie must 403, not 200 popup-close, got {rejected}: {rejected_body}"
    );
    let rejected_lower = rejected_body.to_ascii_lowercase();
    assert!(
        rejected_lower.contains("user mismatch"),
        "user-mismatch must say OAuth state user mismatch, got {rejected_body}"
    );
    assert!(
        !rejected_lower.contains("workspace mismatch"),
        "user-mismatch must not be labeled workspace mismatch: {rejected_body}"
    );

    assert!(
        creds_on(&pool, ws_b, &key).await.is_none(),
        "must not encrypt tokens onto workspace B"
    );
    assert!(
        creds_on(&pool, ws_a, &key).await.is_none(),
        "rejected mismatch must not complete Connect onto A either"
    );
    assert!(
        creds_on(&pool, ws_a2, &key).await.is_none(),
        "rejected mismatch must not encrypt onto A's other workspace"
    );

    // Matching session: same user, state workspace A. Tokens bind to A,
    // never to the other workspace this user also belongs to.
    let jwt_ok = issue_oauth_state(&secret, user_a, ws_a, "trello");
    let ok_tok = format!("tok_ws_a_{}", Uuid::now_v7());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;
    let ok = oauth_callback(&app, &a_cookie, &ok_tok, &jwt_ok).await;
    assert_eq!(
        ok,
        StatusCode::OK,
        "state A + session A must complete Connect via oauth_callback, got {ok}"
    );

    let stored_a = creds_on(&pool, ws_a, &key)
        .await
        .expect("tokens must be encrypted onto the state workspace A");
    assert_eq!(stored_a["access_token"], ok_tok);
    assert!(
        creds_on(&pool, ws_b, &key).await.is_none(),
        "successful Connect for A must not encrypt tokens onto B"
    );
    assert!(
        creds_on(&pool, ws_a2, &key).await.is_none(),
        "successful Connect for A must not encrypt tokens onto A's other workspace"
    );

    assert_ne!(user_a, user_b);
    assert_ne!(ws_a, ws_b);
    assert_ne!(ws_a, ws_a2);
}

/// Stolen `state` JWT + `code` (logs, Referer, a copied callback URL) must
/// not complete Connect. SameSite=Lax sends `fleet_access` on a real
/// top-level provider redirect; a missing cookie is not a guest Connect.
/// Refresh-only (`fleet_refresh`, no `fleet_access`) is the same as no
/// session. Drive the HTTP wrapper so 200 popup-close-as-connected cannot
/// hide an inner Ok.
#[tokio::test]
async fn oauth_callback_requires_session_cookie() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "sess-a").await;
    // Second workspace so a successful control cannot be "tokens landed
    // somewhere this user owns" — they must bind to the state workspace.
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
    let a_cookie = session_cookie(&state, user_a, &email_a).await;

    // 1. Product start: issue a valid Trello state for owner A. The
    // callback persists the nonce (insert = consume). Do not pre-insert
    // here — that would make no-cookie fail as reuse and hide the hole.
    let jwt_no_cookie = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "trello")
        .expect("issue product oauth state");
    let claims_no_cookie =
        verify_oauth_state(&secret, &jwt_no_cookie).expect("issued state must verify");
    assert!(
        Uuid::parse_str(&claims_no_cookie.nonce).is_ok(),
        "state nonce must be persistable as a UUID, got {}",
        claims_no_cookie.nonce
    );
    let stolen = format!("tok_stolen_no_cookie_{}", Uuid::now_v7());
    let (no_cookie_status, no_cookie_body) =
        oauth_callback_http(&app, None, &stolen, &jwt_no_cookie).await;
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

    // 2. Refresh-only cookie is not a session. Must not upsert.
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

    // Nonce: reject-before-consume may leave the first JWT reusable; if
    // the unauthenticated attempt consumed it, a later matching-cookie
    // retry of that JWT would fail as reuse. Either is allowed — a fresh
    // nonce is required after a no-cookie attempt, and A's matching-cookie
    // Connect with a fresh nonce must still succeed.
    let jwt_ok = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "trello")
        .expect("issue fresh control state");
    assert_ne!(
        verify_oauth_state(&secret, &jwt_ok)
            .expect("fresh control state")
            .nonce,
        claims_no_cookie.nonce,
        "control must use a fresh nonce after the no-cookie attempt"
    );

    // 3. Control so this is not a tautology: A cookie + A state stores
    // access_token on A only (same as the bind card's happy path).
    let ok_tok = format!("tok_sess_a_{}", Uuid::now_v7());
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
        "A cookie + A state must complete Connect via oauth_callback, got {ok_status}: {ok_body}"
    );
    let stored_a = creds_on(&pool, ws_a, &key)
        .await
        .expect("tokens must be encrypted onto the state workspace A");
    assert_eq!(stored_a["access_token"], ok_tok);
    assert_ne!(stored_a["access_token"], stolen);
    assert_ne!(stored_a["access_token"], refresh_tok);
    assert!(
        creds_on(&pool, ws_a2, &key).await.is_none(),
        "successful Connect for A must not encrypt tokens onto A's other workspace"
    );
}

/// Expired OAuth `state` JWT must not complete Connect. `verify_oauth_state`
/// is a unit only — this drives `oauth_callback` so a handler that skipped
/// verify, set `validate_exp = false`, or upserted before verify cannot pass.
/// Nonce is not consumed on verify failure (list-oauth-state-reuse owns
/// consume-once of a still-valid nonce).
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
    let client_secret = state
        .config
        .trello
        .as_ref()
        .expect("trello test app")
        .client_secret
        .clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;

    // 1. Product start: issue a valid Trello state for owner A and keep the
    // nonce. Do not pre-insert — consume is the callback insert. Then mint
    // a JWT with the same sub/workspace_id/provider/nonce/typ but exp past.
    let issued = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "trello")
        .expect("issue product oauth state");
    let claims = verify_oauth_state(&secret, &issued).expect("issued state must verify");
    assert_eq!(claims.sub, user_a.to_string());
    assert_eq!(claims.workspace_id, ws_a.to_string());
    assert_eq!(claims.provider, "trello");
    assert_eq!(claims.typ, "oauth_state");
    assert!(
        Uuid::parse_str(&claims.nonce).is_ok(),
        "state nonce must be persistable as a UUID, got {}",
        claims.nonce
    );

    let now = Utc::now().timestamp();
    let expired_jwt = encode_oauth_state(
        &secret,
        user_a,
        ws_a,
        "trello",
        &claims.nonce,
        now - 180,
        -60,
    );
    assert_ne!(expired_jwt, issued, "expired JWT is a new encoding");
    verify_oauth_state(&secret, &expired_jwt).expect_err("expired oauth_state must not verify");

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
        "expired OAuth state must 401 invalid or expired, not 200 popup-close as connected, got {expired_status}: {expired_body}"
    );
    let expired_lower = expired_body.to_ascii_lowercase();
    assert!(
        expired_lower.contains("invalid") || expired_lower.contains("expired"),
        "401 body must say invalid or expired OAuth state, got {expired_body}"
    );
    assert!(
        !expired_lower.contains("ok: true") && !expired_lower.contains("ok:true"),
        "expired state must not popup-close as connected: {expired_body}"
    );
    assert!(
        !expired_lower.contains("trello connected"),
        "expired state must not report provider connected: {expired_body}"
    );
    assert!(
        !expired_body.contains("access_token")
            && !expired_body.contains("refresh_token")
            && !expired_body.contains("client_secret")
            && !expired_body.contains(&expired_tok)
            && !expired_body.contains(&client_secret),
        "reject body must not contain access_token / refresh_token / client_secret: {expired_body}"
    );
    assert!(
        creds_on(&pool, ws_a, &key).await.is_none(),
        "expired OAuth state must not encrypt tokens onto A"
    );
    assert!(
        creds_on(&pool, ws_a2, &key).await.is_none(),
        "expired OAuth state must not encrypt tokens onto A's other workspace"
    );

    // Nonce must not be consumed (verify fails first). A matching-cookie
    // Connect with a fresh unexpired JWT carrying that same nonce must
    // still succeed — proving the expired attempt did not insert/consume.
    let jwt_ok = encode_oauth_state(&secret, user_a, ws_a, "trello", &claims.nonce, now, 600);
    verify_oauth_state(&secret, &jwt_ok).expect("unexpired same-nonce state must verify");

    // 3. Control so this is not a tautology: A cookie + unexpired A state
    // stores access_token on A only (same as the bind card happy path).
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
        "A cookie + unexpired A state must complete Connect after the expired attempt, got {ok_status}: {ok_body}"
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

/// After a real Trello Connect, `integration_connections.credentials` must be
/// an AES-GCM envelope (`_enc=v1`, nonce+ct hex). `credentials_for` decrypts
/// *and* passes through a raw `{access_token}` blob, so a skipped `encrypt()`
/// before upsert still greens the bind / reuse / session cards. SELECT the
/// stored jsonb (find_connected / db credentials_for). GET /integrations and
/// the callback HTML must omit the planted token.
#[tokio::test]
async fn oauth_callback_credentials_encrypted_at_rest() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "enc-a").await;
    let (user_b, email_b, ws_b) = seed_user_with_workspace(&pool, "enc-b").await;
    let ws_a2 = Uuid::now_v7();
    workspaces::create_owned(
        &pool,
        ws_a2,
        user_a,
        &format!("owe-enc-a2-{ws_a2}"),
        "OAuth enc A2",
        Utc::now(),
    )
    .await
    .expect("create second workspace for A");

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let key = state.config.credentials_key.clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;
    let b_cookie = session_cookie(&state, user_b, &email_b).await;

    // Other-ws cookie must 403 and must not write a credentials row on A or B.
    let jwt_steal = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "trello")
        .expect("issue steal-probe oauth state");
    let steal_tok = format!("tok_enc_steal_{}", Uuid::now_v7());
    let (steal_status, steal_body) = oauth_callback_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={b_cookie}")),
        &steal_tok,
        &jwt_steal,
    )
    .await;
    assert_eq!(
        steal_status,
        StatusCode::FORBIDDEN,
        "other-ws cookie on A's Trello callback must 403, got {steal_status}: {steal_body}"
    );
    assert_callback_rejected_without_session(steal_status, &steal_body, "other-ws Trello callback");
    assert!(
        !steal_body.contains(&steal_tok),
        "other-ws reject must not echo the planted token: {steal_body}"
    );
    assert!(
        creds_on(&pool, ws_a, &key).await.is_none()
            && stored_connection(&pool, ws_a).await.is_none(),
        "other-ws cookie must not write credentials onto A"
    );
    assert!(
        creds_on(&pool, ws_b, &key).await.is_none()
            && stored_connection(&pool, ws_b).await.is_none(),
        "other-ws cookie must not write credentials onto B"
    );
    assert!(
        creds_on(&pool, ws_a2, &key).await.is_none(),
        "other-ws cookie must not write credentials onto A's other workspace"
    );

    // Control: A cookie + unexpired A Trello state + code completes Connect
    // without a live provider (code is the access token).
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
            && !ok_body.contains("refresh_token")
            && !ok_body.contains("client_secret"),
        "callback HTML must not contain the planted token or secret field names: {ok_body}"
    );

    let decrypted = creds_on(&pool, ws_a, &key).await.expect(
        "credentials_for(A, trello) must be Some after Connect (control — not a tautology)",
    );
    assert_eq!(
        decrypted["access_token"], ok_tok,
        "credentials_for must decrypt to the planted access_token: {decrypted}"
    );
    assert_ne!(decrypted["access_token"], steal_tok);

    let row = stored_connection(&pool, ws_a)
        .await
        .expect("find_connected must load the Trello row");
    assert_eq!(row.workspace_id, ws_a);
    assert_eq!(row.provider, "trello");
    assert_eq!(row.status, "connected");
    assert_aes_gcm_envelope(&row.credentials, &ok_tok, "find_connected.credentials");
    let raw_db = integ_repo::credentials_for(&pool, ws_a, "trello")
        .await
        .expect("db credentials_for")
        .expect("stored credentials jsonb");
    assert_aes_gcm_envelope(&raw_db, &ok_tok, "integration_connections.credentials");
    let raw_text: String = sqlx::query_scalar(
        r#"
        SELECT credentials::text
        FROM integration_connections
        WHERE workspace_id = $1 AND provider = $2
        "#,
    )
    .bind(ws_a)
    .bind("trello")
    .fetch_one(&pool)
    .await
    .expect("SELECT credentials::text");
    assert!(
        !raw_text.contains(&ok_tok),
        "raw credentials::text must not contain the plaintext token: {raw_text}"
    );
    let raw_json: serde_json::Value =
        serde_json::from_str(&raw_text).expect("credentials::text must be JSON");
    assert_aes_gcm_envelope(&raw_json, &ok_tok, "credentials::text");
    let config_text = row.config.to_string();
    assert!(
        !config_text.contains(&ok_tok),
        "config must not contain the plaintext token: {config_text}"
    );
    if let Some(label) = row.account_label.as_deref() {
        assert!(
            !label.contains(&ok_tok),
            "account_label must not contain the plaintext token: {label}"
        );
    }

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
        "trello must show connected so omit-token is not vacuous: {list_body}"
    );
    assert!(
        !list_body.contains(&ok_tok)
            && !list_body.contains("access_token")
            && !list_body.contains("refresh_token")
            && !list_body.contains("client_secret")
            && !list_body.contains("list_oauth_apps"),
        "GET /integrations must omit the planted token and secret fields: {list_body}"
    );

    assert!(
        creds_on(&pool, ws_b, &key).await.is_none()
            && stored_connection(&pool, ws_b).await.is_none(),
        "successful Connect for A must not write credentials onto B"
    );
    assert!(
        creds_on(&pool, ws_a2, &key).await.is_none()
            && stored_connection(&pool, ws_a2).await.is_none(),
        "successful Connect for A must not write credentials onto A's other workspace"
    );
}

/// Provider mismatch is a typed 400, not 200 popup-close. GitHub state on
/// the Trello callback must not consume or encrypt.
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
        lower.contains("provider mismatch"),
        "400 body must name provider mismatch: {body}"
    );
    assert!(
        !lower.contains("ok: true") && !lower.contains("ok:true"),
        "provider mismatch must not popup-close as connected: {body}"
    );
    assert!(
        creds_on(&pool, ws_a, &key).await.is_none(),
        "provider mismatch must not encrypt tokens onto A"
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

async fn github_manifest_http(
    app: &axum::Router,
    cookie: Option<&str>,
    code: &str,
    state_jwt: &str,
) -> (StatusCode, String) {
    let mut builder = Request::builder().method("GET").uri(format!(
        "/api/integrations/oauth/github/manifest/callback?code={code}&state={state_jwt}"
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

async fn github_oauth_apps(
    pool: &fleet_db::PgPool,
    workspace_id: Uuid,
) -> Vec<fleet_db::IntegrationOAuthApp> {
    integ_repo::list_oauth_apps(pool, workspace_id)
        .await
        .expect("list_oauth_apps")
}

/// Stolen GitHub App manifest `code`+`state` must not persist without a
/// matching `fleet_access` session. Same bind as user-oauth.
#[tokio::test]
async fn github_manifest_callback_requires_session_user_match() {
    let pool = connect_and_migrate().await;
    let (user_a, email_a, ws_a) = seed_user_with_workspace(&pool, "gh-a").await;
    let (user_b, email_b, ws_b) = seed_user_with_workspace(&pool, "gh-b").await;
    workspaces::add_member(&pool, ws_a, user_b, "member", Utc::now())
        .await
        .expect("add B as member of A");

    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let key = state.config.credentials_key.clone();
    let app = fleet_integrations::router().with_state(state.clone());
    let a_cookie = session_cookie(&state, user_a, &email_a).await;
    let b_cookie = session_cookie(&state, user_b, &email_b).await;

    let jwt_a = fleet_integrations::issue_oauth_state(&secret, user_a, ws_a, "github")
        .expect("issue github manifest state");

    // 1. No cookie: stolen code+state must 401 before convert/persist.
    let stolen = format!("manifest-code-stolen-{}", Uuid::now_v7());
    let (no_cookie_status, no_cookie_body) =
        github_manifest_http(&app, None, &stolen, &jwt_a).await;
    assert_eq!(
        no_cookie_status,
        StatusCode::UNAUTHORIZED,
        "no-cookie manifest callback must 401, got {no_cookie_status}: {no_cookie_body}"
    );
    assert_callback_rejected_without_session(
        no_cookie_status,
        &no_cookie_body,
        "no Cookie header on github manifest",
    );
    assert!(
        github_oauth_apps(&pool, ws_a).await.is_empty(),
        "no-cookie manifest must not persist an oauth app onto A"
    );

    // 2. Refresh-only is not a session.
    let refresh_only = format!("{REFRESH_COOKIE}={}", Uuid::now_v7().as_simple());
    let (refresh_status, refresh_body) =
        github_manifest_http(&app, Some(&refresh_only), &stolen, &jwt_a).await;
    assert_eq!(
        refresh_status,
        StatusCode::UNAUTHORIZED,
        "refresh-only manifest callback must 401, got {refresh_status}: {refresh_body}"
    );
    assert!(
        github_oauth_apps(&pool, ws_a).await.is_empty(),
        "refresh-only manifest must not persist an oauth app onto A"
    );

    // 3. B is a member of A; session user still must equal state.sub.
    let (other_status, other_body) = github_manifest_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={b_cookie}")),
        &stolen,
        &jwt_a,
    )
    .await;
    assert_eq!(
        other_status,
        StatusCode::FORBIDDEN,
        "other-user cookie on A's manifest callback must 403, got {other_status}: {other_body}"
    );
    let other_lower = other_body.to_ascii_lowercase();
    assert!(
        other_lower.contains("user mismatch"),
        "manifest user-mismatch must say OAuth state user mismatch, got {other_body}"
    );
    assert!(
        github_oauth_apps(&pool, ws_a).await.is_empty()
            && github_oauth_apps(&pool, ws_b).await.is_empty(),
        "other-user cookie must not persist an oauth app onto A or B"
    );
    assert!(
        fleet_integrations::credentials_for(&pool, ws_a, "github", &key)
            .await
            .expect("load github A")
            .is_none()
            && fleet_integrations::credentials_for(&pool, ws_b, "github", &key)
                .await
                .expect("load github B")
                .is_none(),
        "rejected manifest must not write github credentials"
    );

    // 4. Matching A cookie reaches convert (fake code fails closed) and
    // must not persist. Distinguishes cookie-bind 401/403 from convert.
    let (ok_session_status, ok_session_body) = github_manifest_http(
        &app,
        Some(&format!("{ACCESS_COOKIE}={a_cookie}")),
        &stolen,
        &jwt_a,
    )
    .await;
    assert_ne!(
        ok_session_status,
        StatusCode::UNAUTHORIZED,
        "matching cookie must not 401 as unauthenticated: {ok_session_status}: {ok_session_body}"
    );
    assert_ne!(
        ok_session_status,
        StatusCode::FORBIDDEN,
        "matching cookie must not 403 as user mismatch: {ok_session_status}: {ok_session_body}"
    );
    assert_ne!(
        ok_session_status,
        StatusCode::FOUND,
        "fake manifest code must not 302 to install: {ok_session_status}: {ok_session_body}"
    );
    let ok_lower = ok_session_body.to_ascii_lowercase();
    assert!(
        !ok_lower.contains("ok: true") && !ok_lower.contains("ok:true"),
        "fake manifest code must not popup-close as connected: {ok_session_body}"
    );
    assert!(
        github_oauth_apps(&pool, ws_a).await.is_empty(),
        "fake manifest code must not persist an oauth app"
    );
}
