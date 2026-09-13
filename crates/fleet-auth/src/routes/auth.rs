use std::net::SocketAddr;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use chrono::{Duration as ChronoDuration, Utc};
use fleet_db::repos::{sessions as session_repo, users as user_repo, workspaces as workspace_repo};
use fleet_db::{NewAuthSession, User, UserPublic, WorkspacePublic};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::cookies::{clear_auth_cookies, set_auth_cookies};
use crate::error::{AppError, AppResult};
use crate::models::{AuthResponse, LoginRequest, MeResponse, SignupRequest};
use crate::password::{hash_password, verify_password};
use crate::rate_limit;
use crate::routes::workspaces::{
    default_workspace_name, ensure_default_workspace, workspace_slug_for_user,
};
use crate::state::AuthState;
use crate::tokens::{
    ACCESS_COOKIE, REFRESH_COOKIE, decode_access_token, decode_access_token_for_logout,
    generate_refresh_token, hash_refresh_token, issue_access_token,
};

/// Resolve the authenticated user from access-cookie JWT + live session row.
pub async fn require_user(state: &AuthState, jar: &CookieJar) -> AppResult<User> {
    let access = jar
        .get(ACCESS_COOKIE)
        .map(|c| c.value().to_string())
        .ok_or_else(|| AppError::unauthorized("not authenticated"))?;

    let claims = decode_access_token(&state.config.jwt_secret, &access)?;
    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| AppError::unauthorized("invalid token subject"))?;
    let session_id = Uuid::parse_str(&claims.sid)
        .map_err(|_| AppError::unauthorized("invalid token session"))?;

    if !session_repo::is_session_active(&state.db, session_id, user_id).await? {
        return Err(AppError::unauthorized("session is no longer valid"));
    }

    let user = user_repo::find_active_by_id(&state.db, user_id)
        .await?
        .ok_or_else(|| AppError::unauthorized("user not found"))?;
    grant_env_instance_admin(state, user).await
}

/// Grant `instance_admin` when the email is listed in `INSTANCE_ADMIN_EMAILS`.
/// Never revokes a flag that was set in the UI or a previous grant.
pub async fn grant_env_instance_admin(state: &AuthState, user: User) -> AppResult<User> {
    if user.instance_admin || !state.config.email_is_env_instance_admin(&user.email_normalized) {
        return Ok(user);
    }
    user_repo::set_instance_admin(&state.db, user.id, true)
        .await?
        .ok_or_else(|| AppError::internal("failed to grant instance admin"))
}

/// Cookie user who is an instance admin (after env grant).
pub async fn require_instance_admin(state: &AuthState, jar: &CookieJar) -> AppResult<User> {
    let user = require_user(state, jar).await?;
    if !user.instance_admin {
        return Err(AppError::forbidden("instance admin required"));
    }
    Ok(user)
}

fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

fn validate_credentials(email: &str, password: &str) -> AppResult<()> {
    if email.trim().is_empty() || !email.contains('@') {
        return Err(AppError::bad_request("valid email is required"));
    }
    if password.len() < 8 {
        return Err(AppError::bad_request(
            "password must be at least 8 characters",
        ));
    }
    if password.len() > 128 {
        return Err(AppError::bad_request("password is too long"));
    }
    Ok(())
}

fn client_meta(
    headers: &HeaderMap,
    connect: Option<SocketAddr>,
) -> (Option<String>, Option<String>) {
    let ua = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.chars().take(512).collect());
    (ua, rate_limit::client_ip(headers, connect))
}

/// Create session row + JWT access + opaque refresh. Single-row insert only
/// (no locks on `users`) so concurrent logins never deadlock.
async fn create_session_and_tokens(
    state: &AuthState,
    user: &User,
    headers: &HeaderMap,
    connect: Option<SocketAddr>,
) -> AppResult<(String, String, Uuid)> {
    let session_id = Uuid::now_v7();
    let family_id = Uuid::now_v7();
    let refresh = generate_refresh_token();
    let refresh_hash = hash_refresh_token(&refresh);
    let now = Utc::now();
    let expires_at =
        now + ChronoDuration::from_std(state.config.refresh_ttl).unwrap_or(ChronoDuration::days(7));
    let absolute_expires_at = expires_at;
    let (ua, ip) = client_meta(headers, connect);

    session_repo::insert(
        &state.db,
        &NewAuthSession {
            id: session_id,
            user_id: user.id,
            refresh_token_hash: refresh_hash,
            family_id,
            user_agent: ua,
            ip,
            expires_at,
            absolute_expires_at,
        },
    )
    .await?;

    let access = issue_access_token(
        &state.config.jwt_secret,
        user.id,
        &user.email,
        session_id,
        state.config.access_ttl,
    )?;

    Ok((access, refresh, session_id))
}

fn auth_json_response(
    state: &AuthState,
    status: StatusCode,
    body: AuthResponse,
    access: &str,
    refresh: &str,
) -> Response {
    let mut headers = HeaderMap::new();
    set_auth_cookies(&mut headers, &state.config, access, refresh);
    (status, headers, Json(body)).into_response()
}

fn signup_token_ok(expected: Option<&str>, got: Option<&str>) -> bool {
    match expected.map(str::trim).filter(|s| !s.is_empty()) {
        None => true,
        Some(want) => match got.map(str::trim).filter(|s| !s.is_empty()) {
            None => false,
            Some(got) => {
                let want_h = Sha256::digest(want.as_bytes());
                let got_h = Sha256::digest(got.as_bytes());
                want_h.ct_eq(&got_h).into()
            }
        },
    }
}

pub async fn signup(
    State(state): State<AuthState>,
    rate_limit::ClientAddr(connect): rate_limit::ClientAddr,
    headers: HeaderMap,
    Json(body): Json<SignupRequest>,
) -> AppResult<Response> {
    rate_limit::check(
        "signup",
        &headers,
        connect,
        rate_limit::max_from_env("AUTH_SIGNUP_RATE_MAX", 5),
        rate_limit::window_from_env("AUTH_SIGNUP_RATE_WINDOW_SECS", 15 * 60),
    )?;
    if !signup_token_ok(state.config.signup_token.as_deref(), body.token.as_deref()) {
        return Err(AppError::forbidden("valid signup token is required"));
    }
    validate_credentials(&body.email, &body.password)?;
    let email = body.email.trim().to_string();
    let email_normalized = normalize_email(&email);
    tracing::debug!(email = %email_normalized, "signup attempt");
    let password_hash = hash_password(&body.password)?;
    let user_id = Uuid::now_v7();
    let display_name = body
        .display_name
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let now = Utc::now();

    let workspace_id = Uuid::now_v7();
    let slug = workspace_slug_for_user(&email_normalized, user_id);
    let workspace_name = default_workspace_name(display_name.as_deref(), &email_normalized);

    let mut tx = state.db.begin().await.map_err(fleet_db::DbError::from)?;

    let user = user_repo::insert_tx(
        &mut tx,
        &user_repo::NewUser {
            id: user_id,
            email,
            email_normalized: email_normalized.clone(),
            display_name: display_name.clone(),
            password_hash,
            created_at: now,
        },
    )
    .await?;

    workspace_repo::create_personal_workspace_tx(
        &mut tx,
        workspace_id,
        user_id,
        &slug,
        &workspace_name,
        now,
    )
    .await?;

    tx.commit().await.map_err(fleet_db::DbError::from)?;

    let user = grant_env_instance_admin(&state, user).await?;

    if let Err(e) =
        workspace_repo::accept_pending_invites_for_user(&state.db, user_id, &email_normalized, now)
            .await
    {
        tracing::warn!(
            user_id = %user_id,
            error = %e,
            "signup: accept pending invites failed"
        );
    }

    let _ = headers;
    tracing::info!(
        user_id = %user_id,
        email = %email_normalized,
        workspace_id = %workspace_id,
        "signup succeeded"
    );

    Ok((
        StatusCode::CREATED,
        Json(AuthResponse {
            user: UserPublic::from(user),
            message: "account created — please log in".into(),
            default_workspace: Some(WorkspacePublic {
                id: workspace_id,
                slug,
                name: workspace_name,
                role: "owner".into(),
            }),
        }),
    )
        .into_response())
}

pub async fn login(
    State(state): State<AuthState>,
    rate_limit::ClientAddr(connect): rate_limit::ClientAddr,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> AppResult<Response> {
    rate_limit::check(
        "login",
        &headers,
        connect,
        rate_limit::max_from_env("AUTH_LOGIN_RATE_MAX", 10),
        rate_limit::window_from_env("AUTH_LOGIN_RATE_WINDOW_SECS", 15 * 60),
    )?;
    validate_credentials(&body.email, &body.password)?;
    let email_normalized = normalize_email(&body.email);
    tracing::debug!(email = %email_normalized, "login attempt");

    let user = user_repo::find_by_email_normalized(&state.db, &email_normalized)
        .await?
        .ok_or_else(|| {
            tracing::warn!(email = %email_normalized, "login failed: unknown email");
            AppError::unauthorized("invalid email or password")
        })?;

    if !user.is_active() {
        tracing::warn!(user_id = %user.id, "login failed: account inactive");
        return Err(AppError::unauthorized("account is not active"));
    }

    if !verify_password(&body.password, &user.password_hash)? {
        tracing::warn!(user_id = %user.id, email = %email_normalized, "login failed: bad password");
        return Err(AppError::unauthorized("invalid email or password"));
    }

    let user = grant_env_instance_admin(&state, user).await?;

    let (access, refresh, sid) =
        create_session_and_tokens(&state, &user, &headers, connect).await?;

    if let Err(e) = workspace_repo::accept_pending_invites_for_user(
        &state.db,
        user.id,
        &email_normalized,
        Utc::now(),
    )
    .await
    {
        tracing::warn!(
            user_id = %user.id,
            error = %e,
            "login: accept pending invites failed"
        );
    }

    let default_ws =
        ensure_default_workspace(&state, user.id, &user.email, user.display_name.as_deref())
            .await?;

    tracing::info!(
        user_id = %user.id,
        session_id = %sid,
        workspace_id = %default_ws.id,
        "login succeeded"
    );

    Ok(auth_json_response(
        &state,
        StatusCode::OK,
        AuthResponse {
            user: UserPublic::from(user),
            message: "logged in".into(),
            default_workspace: Some(WorkspacePublic::from(default_ws)),
        },
        &access,
        &refresh,
    ))
}

pub async fn logout(State(state): State<AuthState>, jar: CookieJar) -> AppResult<Response> {
    let now = Utc::now();
    if let Some(refresh) = jar.get(REFRESH_COOKIE).map(|c| c.value().to_string()) {
        let hash = hash_refresh_token(&refresh);
        session_repo::revoke_by_refresh_hash(&state.db, &hash, now).await?;
    } else if let Some(access) = jar.get(ACCESS_COOKIE).map(|c| c.value().to_string())
        && let Ok(claims) = decode_access_token_for_logout(&state.config.jwt_secret, &access)
        && let Ok(sid) = Uuid::parse_str(&claims.sid)
    {
        // Refresh cookie missing (cleared, blocked, or already rotated).
        // Still revoke the session named in the access JWT, even if `exp` passed.
        session_repo::revoke_by_id(&state.db, sid, now).await?;
    }

    let mut headers = HeaderMap::new();
    clear_auth_cookies(&mut headers, &state.config);
    Ok((
        StatusCode::OK,
        headers,
        Json(serde_json::json!({ "message": "logged out" })),
    )
        .into_response())
}

pub async fn refresh(
    State(state): State<AuthState>,
    rate_limit::ClientAddr(connect): rate_limit::ClientAddr,
    headers: HeaderMap,
    jar: CookieJar,
) -> AppResult<Response> {
    rate_limit::check(
        "refresh",
        &headers,
        connect,
        rate_limit::max_from_env("AUTH_REFRESH_RATE_MAX", 30),
        rate_limit::window_from_env("AUTH_REFRESH_RATE_WINDOW_SECS", 15 * 60),
    )?;
    let refresh = jar
        .get(REFRESH_COOKIE)
        .map(|c| c.value().to_string())
        .ok_or_else(|| AppError::unauthorized("missing refresh token"))?;

    let hash = hash_refresh_token(&refresh);
    let now = Utc::now();

    let session = session_repo::find_by_refresh_hash(&state.db, &hash)
        .await?
        .ok_or_else(|| AppError::unauthorized("invalid refresh token"))?;

    if session.revoked_at.is_some() {
        session_repo::revoke_family(&state.db, session.family_id, now).await?;
        return Err(AppError::unauthorized(
            "refresh token reuse detected — session family revoked",
        ));
    }

    if session.expires_at < now || session.absolute_expires_at < now {
        session_repo::revoke_by_id(&state.db, session.id, now).await?;
        return Err(AppError::unauthorized("refresh token expired"));
    }

    let user = user_repo::find_by_id(&state.db, session.user_id)
        .await?
        .ok_or_else(|| AppError::unauthorized("user not found"))?;

    if !user.is_active() {
        return Err(AppError::unauthorized("account is not active"));
    }

    let user = grant_env_instance_admin(&state, user).await?;

    let new_session_id = Uuid::now_v7();
    let new_refresh = generate_refresh_token();
    let new_hash = hash_refresh_token(&new_refresh);
    let new_expires =
        now + ChronoDuration::from_std(state.config.refresh_ttl).unwrap_or(ChronoDuration::days(7));
    let (ua, ip) = client_meta(&headers, connect);

    let mut tx = state.db.begin().await.map_err(fleet_db::DbError::from)?;

    let revoked = session_repo::revoke_for_rotation_tx(&mut tx, session.id, &hash, now).await?;

    if revoked != 1 {
        tx.rollback().await.ok();
        return Err(AppError::unauthorized(
            "refresh token already rotated — try again",
        ));
    }

    session_repo::insert_tx(
        &mut tx,
        &NewAuthSession {
            id: new_session_id,
            user_id: user.id,
            refresh_token_hash: new_hash,
            family_id: session.family_id,
            user_agent: ua.or(session.user_agent),
            ip,
            expires_at: new_expires.min(session.absolute_expires_at),
            absolute_expires_at: session.absolute_expires_at,
        },
    )
    .await?;

    tx.commit().await.map_err(fleet_db::DbError::from)?;

    let access = issue_access_token(
        &state.config.jwt_secret,
        user.id,
        &user.email,
        new_session_id,
        state.config.access_ttl,
    )?;

    let default_ws =
        ensure_default_workspace(&state, user.id, &user.email, user.display_name.as_deref())
            .await?;

    Ok(auth_json_response(
        &state,
        StatusCode::OK,
        AuthResponse {
            user: UserPublic::from(user),
            message: "token refreshed".into(),
            default_workspace: Some(WorkspacePublic::from(default_ws)),
        },
        &access,
        &new_refresh,
    ))
}

pub async fn me(State(state): State<AuthState>, jar: CookieJar) -> AppResult<Json<MeResponse>> {
    let user = require_user(&state, &jar).await?;
    // Ensure a default workspace exists for older accounts / edge cases.
    let _ = ensure_default_workspace(&state, user.id, &user.email, user.display_name.as_deref())
        .await?;
    let workspaces = workspace_repo::list_for_user(&state.db, user.id)
        .await?
        .into_iter()
        .map(WorkspacePublic::from)
        .collect();

    Ok(Json(MeResponse {
        user: UserPublic::from(user),
        workspaces,
    }))
}
