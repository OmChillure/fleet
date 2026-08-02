//! List / OAuth start / OAuth callback / disconnect.

use axum::Json;
use axum::extract::{FromRef, Path, Query, State};
use axum::http::{HeaderName, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
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

/// Provider redirects here. Exchange code, store tokens, then:
/// 1) notify + focus the original Agent Fleet tab
/// 2) close this auth tab
/// Never loads the full SPA here.
async fn oauth_callback(
    State(state): State<IntegrationsState>,
    Path(provider): Path<String>,
    Query(q): Query<OAuthCallbackQuery>,
) -> Response {
    let ui_origin = state.config.ui_public_base.clone();
    match oauth_callback_inner(&state, &provider, q).await {
        Ok(provider_id) => oauth_popup_close_html(&ui_origin, Some(&provider_id), None),
        Err((workspace_hint, msg)) => {
            tracing::warn!(provider = %provider, error = %msg, "oauth callback failed");
            let _ = workspace_hint;
            oauth_popup_close_html(&ui_origin, None, Some(&msg))
        }
    }
}

/// Auth-tab only: tell opener auth finished, focus it, close this tab.
fn oauth_popup_close_html(
    ui_origin: &str,
    connected: Option<&str>,
    error: Option<&str>,
) -> Response {
    let ui_origin_js = json_escape(ui_origin.trim_end_matches('/'));
    let (ok_js, provider_js, error_js, title, body) = if let Some(p) = connected {
        (
            "true",
            json_escape(p),
            String::new(),
            "Connected",
            format!("{p} connected"),
        )
    } else {
        let err = error.unwrap_or("authorization failed");
        (
            "false",
            String::new(),
            json_escape(err),
            "Connection failed",
            format!("Could not connect: {err}"),
        )
    };

    // Instant page: no Fleet UI. Focus original tab → close auth tab.
    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>{title}</title>
  <style>
    html,body{{margin:0;min-height:100%;background:#0a0a0a;color:#e5e5e5;
      font:15px/1.4 system-ui,-apple-system,sans-serif}}
    main{{min-height:100vh;display:flex;flex-direction:column;align-items:center;
      justify-content:center;gap:10px;padding:24px;text-align:center}}
    p{{margin:0;color:#a3a3a3;max-width:22rem}}
    button{{margin-top:12px;cursor:pointer;border:1px solid #404040;background:#171717;
      color:#f5f5f5;border-radius:8px;padding:10px 16px;font:inherit}}
  </style>
</head>
<body>
<main>
  <div id="msg" style="font-weight:600;font-size:1.05rem">{body_esc}</div>
  <p id="hint">Returning to Agent Fleet…</p>
  <button type="button" id="closeBtn" hidden>Close this tab</button>
</main>
<script>
(function () {{
  var UI = "{ui_origin_js}";
  var payload = {{
    source: "fleet-oauth",
    ok: {ok_js},
    provider: "{provider_js}",
    error: "{error_js}"
  }};

  function notifyOriginal() {{
    try {{
      if (window.opener && !window.opener.closed) {{
        try {{ window.opener.postMessage(payload, UI); }} catch (e) {{}}
        try {{ window.opener.postMessage(payload, "*"); }} catch (e) {{}}
        try {{ window.opener.focus(); }} catch (e) {{}}
        return true;
      }}
    }} catch (e) {{}}
    return false;
  }}

  function closeAuthTab() {{
    try {{ window.close(); }} catch (e) {{}}
    // Some browsers ignore close(); blank + close again
    try {{ window.open("", "_self"); window.close(); }} catch (e) {{}}
  }}

  var hasOpener = notifyOriginal();
  closeAuthTab();
  setTimeout(closeAuthTab, 50);
  setTimeout(closeAuthTab, 200);
  setTimeout(closeAuthTab, 500);

  // If still open, show manual close (do NOT load the Fleet app here).
  setTimeout(function () {{
    if (window.closed) return;
    var hint = document.getElementById("hint");
    var btn = document.getElementById("closeBtn");
    if (hint) {{
      hint.textContent = hasOpener
        ? "You can close this tab and continue in Agent Fleet."
        : "Authorization finished. Close this tab and go back to Agent Fleet.";
    }}
    if (btn) {{
      btn.hidden = false;
      btn.onclick = closeAuthTab;
    }}
  }}, 600);
}})();
</script>
</body>
</html>"#,
        title = title,
        body_esc = html_escape(&body),
        ui_origin_js = ui_origin_js,
        ok_js = ok_js,
        provider_js = provider_js,
        error_js = error_js,
    );

    let mut res = Html(html).into_response();
    *res.status_mut() = StatusCode::OK;
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    // Keep window.opener so we can focus the original tab and close this one.
    res.headers_mut().insert(
        HeaderName::from_static("cross-origin-opener-policy"),
        HeaderValue::from_static("unsafe-none"),
    );
    res
}

fn json_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '\\' => "\\\\".chars().collect::<Vec<_>>(),
            '"' => "\\\"".chars().collect(),
            '\n' => "\\n".chars().collect(),
            '\r' => "\\r".chars().collect(),
            '\t' => "\\t".chars().collect(),
            '<' => "\\u003c".chars().collect(), // avoid </script>
            c => vec![c],
        })
        .collect()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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

    Ok(provider)
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


