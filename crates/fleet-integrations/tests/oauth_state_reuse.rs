//! First callback consumes the OAuth state nonce; replay must not Connect again.
//! Requires DATABASE_URL — never skips.

use std::time::Duration;

use axum_extra::extract::cookie::{Cookie, CookieJar};
use chrono::Utc;
use fleet_auth::ACCESS_COOKIE;
use fleet_db::NewAuthSession;
use fleet_db::repos::{integrations as integ_repo, sessions as session_repo, users, workspaces};
use fleet_integrations::{
    CredentialsKey, IntegrationsConfig, IntegrationsState, OAuthCallbackError, OAuthCallbackQuery,
    OAuthClientConfig, credentials_for, issue_oauth_state, oauth_callback_inner,
    verify_oauth_state,
};
use jsonwebtoken::{EncodingKey, Header, encode};
use serde::Serialize;
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

async fn seed_user_with_workspace(pool: &fleet_db::PgPool, tag: &str) -> (Uuid, String, Uuid) {
    let user_id = Uuid::now_v7();
    let email = format!("oauth.state.reuse.{tag}.{user_id}@example.test");
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
    // UUID v7's first 8 hex chars share a ~65s timestamp bucket; use the
    // whole id so parallel Connect tests do not collide on workspaces.slug.
    let slug = format!("osr-{tag}-{}", workspace_id.as_simple());
    workspaces::create_owned(pool, workspace_id, user_id, &slug, "OAuth state reuse", now)
        .await
        .expect("create workspace");
    (user_id, email, workspace_id)
}

async fn session_jar(state: &IntegrationsState, user_id: Uuid, email: &str) -> CookieJar {
    let session_id = Uuid::now_v7();
    let now = Utc::now();
    session_repo::insert(
        &state.db,
        &NewAuthSession {
            id: session_id,
            user_id,
            refresh_token_hash: session_id.as_bytes().to_vec(),
            family_id: Uuid::now_v7(),
            user_agent: Some("oauth-state-reuse-test".into()),
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
    let jwt = encode(
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
    .expect("access jwt");
    CookieJar::new().add(Cookie::new(ACCESS_COOKIE, jwt))
}

fn test_state(pool: fleet_db::PgPool) -> IntegrationsState {
    IntegrationsState {
        db: pool,
        auth: fleet_auth::AuthConfig::from_env().expect("auth config"),
        config: IntegrationsConfig {
            api_public_base: "http://127.0.0.1:8080".into(),
            ui_public_base: "http://127.0.0.1:3030".into(),
            state_secret: "oauth-state-reuse-test-secret-32chars!".into(),
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

fn callback_query(code: &str, state_jwt: &str) -> OAuthCallbackQuery {
    OAuthCallbackQuery {
        code: Some(code.to_string()),
        state: Some(state_jwt.to_string()),
        error: None,
        error_description: None,
        installation_id: None,
        setup_action: None,
    }
}

fn encode_state_with_nonce(
    secret: &str,
    user_id: Uuid,
    workspace_id: Uuid,
    provider: &str,
    nonce: &str,
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

    let now = Utc::now().timestamp();
    encode(
        &Header::default(),
        &Claims {
            sub: user_id.to_string(),
            workspace_id: workspace_id.to_string(),
            provider: provider.to_string(),
            nonce: nonce.to_string(),
            exp: now + 600,
            iat: now,
            typ: "oauth_state".into(),
        },
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .expect("encode oauth state")
}

async fn stored_access_token(
    pool: &fleet_db::PgPool,
    workspace_id: Uuid,
    key: &CredentialsKey,
) -> Option<String> {
    credentials_for(pool, workspace_id, "trello", key)
        .await
        .expect("load creds")
        .map(|v| {
            v["access_token"]
                .as_str()
                .expect("access_token string")
                .to_string()
        })
}

/// Fail-closed is the consumed nonce, not a generic JWT "invalid"/"expired".
fn replay_rejected(err: &OAuthCallbackError) {
    assert_eq!(
        err.status(),
        Some(axum::http::StatusCode::CONFLICT),
        "nonce reuse must be 409, got {:?} {}",
        err.status(),
        err.message()
    );
    let msg = err.message().to_ascii_lowercase();
    assert!(
        msg.contains("used")
            || msg.contains("replay")
            || msg.contains("already")
            || msg.contains("consumed")
            || (msg.contains("nonce") && !msg.contains("invalid") && !msg.contains("expired")),
        "replay must fail closed on the consumed nonce, got {}",
        err.message()
    );
}

/// Trello treats `code` as the token (no live provider). First Connect
/// must consume the state nonce so a replay cannot attach or replace it.
#[tokio::test]
async fn oauth_state_nonce_must_not_complete_connect_twice() {
    let pool = connect_and_migrate().await;
    let (user_id, email, workspace_id) = seed_user_with_workspace(&pool, "once").await;
    let state = test_state(pool.clone());
    let secret = state.config.state_secret.clone();
    let key = state.config.credentials_key.clone();
    let jar = session_jar(&state, user_id, &email).await;

    let jwt = issue_oauth_state(&secret, user_id, workspace_id, "trello").unwrap();
    let claims = verify_oauth_state(&secret, &jwt).expect("signature check is not consume");
    assert!(!claims.nonce.trim().is_empty());

    // GitHub App manifest verifies the same JWT, then user-oauth completes.
    // Verify-only must not burn the nonce.
    let first_tok = format!("tok_first_{}", Uuid::now_v7());
    let first = oauth_callback_inner(&state, "trello", callback_query(&first_tok, &jwt), &jar)
        .await
        .expect("first Connect must succeed after a prior verify");
    assert_eq!(first, "trello");

    assert_eq!(
        stored_access_token(&pool, workspace_id, &key)
            .await
            .as_deref(),
        Some(first_tok.as_str()),
        "first callback must store that access_token"
    );

    let replay_tok = format!("tok_replay_{}", Uuid::now_v7());
    let replay = oauth_callback_inner(&state, "trello", callback_query(&replay_tok, &jwt), &jar)
        .await
        .expect_err("same state JWT must not complete Connect a second time");
    replay_rejected(&replay);
    assert_eq!(
        stored_access_token(&pool, workspace_id, &key)
            .await
            .as_deref(),
        Some(first_tok.as_str()),
        "replaying the same state JWT must not replace the first token"
    );

    let forged = encode_state_with_nonce(&secret, user_id, workspace_id, "trello", &claims.nonce);
    assert_ne!(forged, jwt);
    let forged_tok = format!("tok_forged_{}", Uuid::now_v7());
    let forged_err =
        oauth_callback_inner(&state, "trello", callback_query(&forged_tok, &forged), &jar)
            .await
            .expect_err("same nonce in a new JWT must not complete Connect again");
    replay_rejected(&forged_err);
    assert_eq!(
        stored_access_token(&pool, workspace_id, &key)
            .await
            .as_deref(),
        Some(first_tok.as_str()),
        "a new JWT carrying the same nonce must not replace the first token"
    );

    integ_repo::disconnect(&pool, workspace_id, "trello")
        .await
        .expect("disconnect");
    assert!(
        stored_access_token(&pool, workspace_id, &key)
            .await
            .is_none(),
        "disconnect must clear credentials"
    );

    let after_disc_tok = format!("tok_after_disc_{}", Uuid::now_v7());
    let after_disc = oauth_callback_inner(
        &state,
        "trello",
        callback_query(&after_disc_tok, &jwt),
        &jar,
    )
    .await
    .expect_err("consumed nonce must stay dead after disconnect");
    replay_rejected(&after_disc);
    assert!(
        stored_access_token(&pool, workspace_id, &key)
            .await
            .is_none(),
        "consumed nonce must not reconnect; credentials stay None"
    );

    let fresh = issue_oauth_state(&secret, user_id, workspace_id, "trello").unwrap();
    let fresh_claims = verify_oauth_state(&secret, &fresh).expect("fresh state");
    assert_ne!(fresh_claims.nonce, claims.nonce);
    let reconnect_tok = format!("tok_reconnect_{}", Uuid::now_v7());
    let reconnect = oauth_callback_inner(
        &state,
        "trello",
        callback_query(&reconnect_tok, &fresh),
        &jar,
    )
    .await
    .expect("a new nonce must be allowed to Connect again");
    assert_eq!(reconnect, "trello");
    assert_eq!(
        stored_access_token(&pool, workspace_id, &key)
            .await
            .as_deref(),
        Some(reconnect_tok.as_str()),
        "a fresh nonce must store the new access_token"
    );
}

#[tokio::test]
async fn oauth_state_parallel_callback_consumes_nonce_once() {
    let pool = connect_and_migrate().await;
    let (user_id, email, workspace_id) = seed_user_with_workspace(&pool, "par").await;
    let state = test_state(pool.clone());
    let key = state.config.credentials_key.clone();
    let jwt =
        issue_oauth_state(&state.config.state_secret, user_id, workspace_id, "trello").unwrap();
    let jar_a = session_jar(&state, user_id, &email).await;
    let jar_b = session_jar(&state, user_id, &email).await;

    let tok_a = format!("tok_par_a_{}", Uuid::now_v7());
    let tok_b = format!("tok_par_b_{}", Uuid::now_v7());
    let (a, b) = tokio::join!(
        oauth_callback_inner(&state, "trello", callback_query(&tok_a, &jwt), &jar_a),
        oauth_callback_inner(&state, "trello", callback_query(&tok_b, &jwt), &jar_b),
    );

    let wins = [a.is_ok(), b.is_ok()].iter().filter(|ok| **ok).count();
    assert_eq!(
        wins, 1,
        "exactly one parallel callback may complete Connect: a={a:?} b={b:?}"
    );
    let loser = if a.is_ok() {
        b.as_ref().unwrap_err()
    } else {
        a.as_ref().unwrap_err()
    };
    replay_rejected(loser);

    let token = stored_access_token(&pool, workspace_id, &key)
        .await
        .expect("one connection");
    if a.is_ok() {
        assert_eq!(token, tok_a, "stored token must be the winner only");
        assert_ne!(token, tok_b);
    } else {
        assert_eq!(token, tok_b, "stored token must be the winner only");
        assert_ne!(token, tok_a);
    }
}
