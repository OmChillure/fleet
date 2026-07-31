//! List / OAuth start / OAuth callback / disconnect.

use axum::Json;
use axum::extract::{FromRef, Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use axum_extra::extract::CookieJar;
use chrono::Utc;
use fleet_auth::{AppError, AppResult, require_user};
use fleet_db::repos::{integrations as integ_repo, workspaces as workspace_repo};
use fleet_db::{IntegrationConnectionPublic, repos::integrations::UpsertConnection};
use serde::Deserialize;
use uuid::Uuid;

use crate::catalog::{self, find_provider};
use crate::models::{
    DisconnectResponse, IntegrationAvailability, IntegrationListResponse, IntegrationView,
};
use crate::oauth::{authorize_url, exchange_code, issue_oauth_state, verify_oauth_state};
use crate::state::IntegrationsState;

/// Workspace integrations + OAuth. Host must provide [`IntegrationsState`].
pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    IntegrationsState: FromRef<S>,
{
    Router::new()
        .route(
            "/api/workspaces/{workspace_id}/integrations",
            get(list_integrations),
        )
        .route(
            "/api/workspaces/{workspace_id}/integrations/{provider}/oauth/start",
            get(oauth_start),
        )
        .route(
            "/api/integrations/oauth/callback/{provider}",
            get(oauth_callback),
        )
        .route(
            "/api/workspaces/{workspace_id}/integrations/{provider}",
            axum::routing::delete(disconnect_integration),
        )
}

async fn require_workspace_member(
    state: &IntegrationsState,
    jar: &CookieJar,
    workspace_id: Uuid,
) -> AppResult<(fleet_db::User, fleet_db::WorkspaceWithRole)> {
    let auth = state.auth_state();
    let user = require_user(&auth, jar).await?;
    let ws = workspace_repo::find_for_user(&state.db, workspace_id, user.id)
        .await?
        .ok_or_else(|| AppError::forbidden("not a member of this workspace"))?;
    Ok((user, ws))
}

fn enrich_views(
    workspace_id: Uuid,
    connections: &[fleet_db::IntegrationConnection],
    state: &IntegrationsState,
) -> Vec<IntegrationView> {
    let by_provider: std::collections::HashMap<&str, &fleet_db::IntegrationConnection> =
        connections.iter().map(|c| (c.provider.as_str(), c)).collect();

    catalog::catalog()
        .into_iter()
        .map(|mut view| {
            let configured = state.config.is_oauth_configured(&view.id);
            view.oauth_configured = configured;
            if matches!(view.availability, IntegrationAvailability::Available) && configured {
                view.oauth_start_path = Some(format!(
                    "/api/workspaces/{workspace_id}/integrations/{}/oauth/start",
                    view.id
                ));
            }
            if let Some(conn) = by_provider.get(view.id.as_str()) {
                view.connected = conn.status == "connected";
                view.connection = Some(IntegrationConnectionPublic::from(*conn));
            }
            view
        })
        .collect()
}

/// `GET /api/workspaces/:workspace_id/integrations`
async fn list_integrations(
    State(state): State<IntegrationsState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<IntegrationListResponse>> {
    let _ = require_workspace_member(&state, &jar, workspace_id).await?;
    let connections = integ_repo::list_for_workspace(&state.db, workspace_id).await?;
    Ok(Json(IntegrationListResponse {
        workspace_id: workspace_id.to_string(),
        integrations: enrich_views(workspace_id, &connections, &state),
    }))
}

/// `GET …/oauth/start` — 302 to GitHub/Slack/… (user must be logged in).
async fn oauth_start(
    State(state): State<IntegrationsState>,
    jar: CookieJar,
    Path((workspace_id, provider)): Path<(Uuid, String)>,
) -> AppResult<Response> {
    let (user, _) = require_workspace_member(&state, &jar, workspace_id).await?;
    let provider = provider.trim().to_lowercase();

    let entry = find_provider(&provider)
        .ok_or_else(|| AppError::not_found(format!("unknown integration: {provider}")))?;

    if !matches!(entry.availability, IntegrationAvailability::Available) {
        return Err(AppError::bad_request(format!(
            "{provider} is not available yet"
        )));
    }

    let client = state.config.oauth_for(&provider).ok_or_else(|| {
        AppError::bad_request(format!(
            "{provider} OAuth is not configured on the server. Set {}_CLIENT_ID and {}_CLIENT_SECRET in the environment.",
            provider.to_uppercase(),
            provider.to_uppercase()
        ))
    })?;

    let oauth_state = issue_oauth_state(
        &state.config.state_secret,
        user.id,
        workspace_id,
        &provider,
    )?;
    let url = authorize_url(&provider, &state.config, client, &oauth_state)?;

    tracing::info!(
        %workspace_id,
        provider = %provider,
        user_id = %user.id,
        "oauth start"
    );

    Ok(Redirect::temporary(&url).into_response())
}

#[derive(Debug, Deserialize)]
struct OAuthCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// Provider redirects here. Exchange code, store tokens, bounce to UI.
async fn oauth_callback(
    State(state): State<IntegrationsState>,
    Path(provider): Path<String>,
    Query(q): Query<OAuthCallbackQuery>,
) -> Response {
    match oauth_callback_inner(&state, &provider, q).await {
        Ok(redirect_url) => redirect_see_other(&redirect_url),
        Err((workspace_hint, msg)) => {
            tracing::warn!(provider = %provider, error = %msg, "oauth callback failed");
            let ws = workspace_hint.unwrap_or_else(|| "unknown".into());
            let url = state.config.ui_integrations_url(
                &ws,
                &format!(
                    "error={}",
                    urlencoding_component(&msg)
                ),
            );
            redirect_see_other(&url)
        }
    }
}

async fn oauth_callback_inner(
    state: &IntegrationsState,
    provider: &str,
    q: OAuthCallbackQuery,
) -> Result<String, (Option<String>, String)> {
    let provider = provider.trim().to_lowercase();

    if let Some(err) = q.error {
        let desc = q.error_description.unwrap_or(err);
        return Err((None, format!("provider denied access: {desc}")));
    }

    let code = q
        .code
        .filter(|s| !s.is_empty())
        .ok_or_else(|| (None, "missing OAuth code".into()))?;
    let state_jwt = q
        .state
        .filter(|s| !s.is_empty())
        .ok_or_else(|| (None, "missing OAuth state".into()))?;

    let claims = verify_oauth_state(&state.config.state_secret, &state_jwt)
        .map_err(|e| (None, e.to_string()))?;

    if claims.provider != provider {
        return Err((
            Some(claims.workspace_id.clone()),
            "OAuth state provider mismatch".into(),
        ));
    }

    let workspace_id = Uuid::parse_str(&claims.workspace_id)
        .map_err(|_| (Some(claims.workspace_id.clone()), "bad workspace in state".into()))?;
    let user_id = Uuid::parse_str(&claims.sub)
        .map_err(|_| (Some(claims.workspace_id.clone()), "bad user in state".into()))?;

    // Ensure user still a member (state alone is not enough if membership revoked).
    let member = workspace_repo::find_for_user(&state.db, workspace_id, user_id)
        .await
        .map_err(|e| (Some(claims.workspace_id.clone()), e.to_string()))?;
    if member.is_none() {
        return Err((
            Some(claims.workspace_id.clone()),
            "not a member of this workspace".into(),
        ));
    }

    let client = state
        .config
        .oauth_for(&provider)
        .ok_or_else(|| {
            (
                Some(claims.workspace_id.clone()),
                format!("{provider} OAuth is not configured"),
            )
        })?;

    let tokens = exchange_code(&provider, &state.config, client, &code)
        .await
        .map_err(|e| (Some(claims.workspace_id.clone()), e.to_string()))?;

    let now = Utc::now();
    integ_repo::upsert(
        &state.db,
        &UpsertConnection {
            id: Uuid::now_v7(),
            workspace_id,
            provider: provider.clone(),
            account_label: tokens.account_label,
            config: tokens.config,
            credentials: tokens.credentials,
            connected_by: Some(user_id),
            at: now,
        },
    )
    .await
    .map_err(|e| (Some(claims.workspace_id.clone()), e.to_string()))?;

    tracing::info!(
        %workspace_id,
        provider = %provider,
        user_id = %user_id,
        "oauth connected"
    );

    Ok(state.config.ui_integrations_url(
        &claims.workspace_id,
        &format!("connected={provider}"),
    ))
}

/// `DELETE /api/workspaces/:id/integrations/:provider`
async fn disconnect_integration(
    State(state): State<IntegrationsState>,
    jar: CookieJar,
    Path((workspace_id, provider)): Path<(Uuid, String)>,
) -> AppResult<Json<DisconnectResponse>> {
    let (user, _) = require_workspace_member(&state, &jar, workspace_id).await?;
    let provider = provider.trim().to_lowercase();

    if !catalog::is_known_provider(&provider) {
        return Err(AppError::not_found(format!("unknown integration: {provider}")));
    }

    let disconnected = integ_repo::disconnect(&state.db, workspace_id, &provider).await?;
    if !disconnected {
        return Err(AppError::not_found(format!(
            "{provider} is not connected for this workspace"
        )));
    }

    tracing::info!(
        %workspace_id,
        provider = %provider,
        user_id = %user.id,
        "integration disconnected"
    );

    Ok(Json(DisconnectResponse {
        provider,
        disconnected: true,
    }))
}

fn redirect_see_other(url: &str) -> Response {
    match HeaderValue::from_str(url) {
        Ok(loc) => {
            let mut res = StatusCode::SEE_OTHER.into_response();
            res.headers_mut().insert(header::LOCATION, loc);
            res
        }
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "invalid redirect URL",
        )
            .into_response(),
    }
}

fn urlencoding_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{b:02X}"));
            }
        }
    }
    out
}
