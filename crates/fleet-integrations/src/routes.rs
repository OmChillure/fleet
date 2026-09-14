//! List / OAuth start / OAuth callback / disconnect.

use axum::extract::{Form, FromRef, Path, Query, State};
use axum::http::{header, HeaderName, HeaderValue, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Json;
use axum::Router;
use axum_extra::extract::CookieJar;
use chrono::Utc;
use fleet_auth::{require_user, AppError, AppResult};
use fleet_db::repos::{integrations as integ_repo, workspaces as workspace_repo};
use fleet_db::{repos::integrations::UpsertConnection, IntegrationConnectionPublic};
use serde::Deserialize;
use uuid::Uuid;

use crate::catalog::{self, find_provider, oauth_app_create_url};
use crate::config::OAuthClientConfig;
use crate::models::{
    DisconnectResponse, IntegrationAvailability, IntegrationListResponse, IntegrationView,
};
use crate::oauth::{
    authorize_url, convert_manifest, exchange_code, github_install_url, issue_oauth_state,
    manifest_form_html, verify_oauth_state, OAuthCallbackError,
};
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
            "/api/workspaces/{workspace_id}/integrations/{provider}/oauth-app",
            axum::routing::put(put_oauth_app),
        )
        .route(
            "/api/workspaces/{workspace_id}/integrations/{provider}/oauth/start",
            get(oauth_start).post(oauth_start_with_app),
        )
        .route(
            "/api/integrations/oauth/callback/{provider}",
            get(oauth_callback),
        )
        .route(
            "/api/integrations/oauth/github/manifest/callback",
            get(github_manifest_callback),
        )
        .route(
            "/api/workspaces/{workspace_id}/integrations/{provider}",
            axum::routing::delete(disconnect_integration).patch(patch_integration_defaults),
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
    workspace_app_providers: &std::collections::HashSet<String>,
    state: &IntegrationsState,
) -> Vec<IntegrationView> {
    let by_provider: std::collections::HashMap<&str, &fleet_db::IntegrationConnection> =
        connections
            .iter()
            .map(|c| (c.provider.as_str(), c))
            .collect();

    catalog::catalog()
        .into_iter()
        .map(|mut view| {
            let configured = state.config.is_oauth_configured(&view.id)
                || workspace_app_providers.contains(&view.id);
            view.oauth_configured = configured;
            view.oauth_callback_url = Some(state.config.callback_url(&view.id));
            view.oauth_app_create_url = oauth_app_create_url(&view.id).map(str::to_string);
            view.oauth_start_path = catalog::oauth_start_path(workspace_id, &view.id);
            if let Some(conn) = by_provider.get(view.id.as_str()) {
                view.connected = conn.status == "connected";
                view.connection = Some(IntegrationConnectionPublic::from(*conn));
            }
            view
        })
        .collect()
}

async fn resolve_oauth_client(
    state: &IntegrationsState,
    workspace_id: Uuid,
    provider: &str,
) -> AppResult<Option<OAuthClientConfig>> {
    if let Some(c) = state.config.oauth_for(provider) {
        return Ok(Some(c.clone()));
    }
    let Some(app) = integ_repo::get_oauth_app(&state.db, workspace_id, provider).await? else {
        return Ok(None);
    };
    let plain = state
        .config
        .credentials_key
        .decrypt(&app.client_secret)
        .map_err(|e| AppError::internal(format!("decrypt oauth app secret: {e}")))?;
    let secret = plain
        .get("client_secret")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::internal("oauth app secret envelope missing client_secret"))?;
    let github_app_slug = plain
        .get("github_app_slug")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Ok(Some(OAuthClientConfig {
        client_id: app.client_id,
        client_secret: secret.to_string(),
        github_app_slug,
    }))
}

/// `GET /api/workspaces/:workspace_id/integrations`
async fn list_integrations(
    State(state): State<IntegrationsState>,
    jar: CookieJar,
    Path(workspace_id): Path<Uuid>,
) -> AppResult<Json<IntegrationListResponse>> {
    let _ = require_workspace_member(&state, &jar, workspace_id).await?;
    let connections = integ_repo::list_for_workspace(&state.db, workspace_id).await?;
    let apps = integ_repo::list_oauth_apps(&state.db, workspace_id).await?;
    let workspace_app_providers = apps.into_iter().map(|a| a.provider).collect();
    Ok(Json(IntegrationListResponse {
        workspace_id: workspace_id.to_string(),
        integrations: enrich_views(workspace_id, &connections, &workspace_app_providers, &state),
    }))
}

#[derive(Debug, Deserialize)]
struct PutOauthAppBody {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, serde::Serialize)]
struct PutOauthAppResponse {
    pub provider: String,
    pub configured: bool,
    pub oauth_start_path: String,
}

/// Save a workspace OAuth application, then the user can start the real consent flow.
async fn put_oauth_app(
    State(state): State<IntegrationsState>,
    jar: CookieJar,
    Path((workspace_id, provider)): Path<(Uuid, String)>,
    Json(body): Json<PutOauthAppBody>,
) -> AppResult<Json<PutOauthAppResponse>> {
    let (user, ws) = require_workspace_member(&state, &jar, workspace_id).await?;
    if !matches!(ws.role.as_str(), "owner" | "admin") {
        return Err(AppError::forbidden(
            "owner or admin required to configure OAuth apps",
        ));
    }
    let provider = provider.trim().to_lowercase();
    let entry = find_provider(&provider)
        .ok_or_else(|| AppError::not_found(format!("unknown integration: {provider}")))?;
    if !matches!(entry.availability, IntegrationAvailability::Available) {
        return Err(AppError::bad_request(format!(
            "{provider} is not available yet"
        )));
    }
    let client_id = body.client_id.trim().to_string();
    let client_secret = body.client_secret.trim().to_string();
    if client_id.is_empty() || client_secret.is_empty() {
        return Err(AppError::bad_request(
            "client_id and client_secret are required",
        ));
    }
    persist_oauth_app(
        &state,
        workspace_id,
        &provider,
        &client_id,
        serde_json::json!({ "client_secret": client_secret }),
        user.id,
    )
    .await?;
    Ok(Json(PutOauthAppResponse {
        provider: provider.clone(),
        configured: true,
        oauth_start_path: format!(
            "/api/workspaces/{workspace_id}/integrations/{provider}/oauth/start"
        ),
    }))
}

/// `GET …/oauth/start` — 302 to GitHub/Jira/… login + consent (user must be logged in).
///
/// If Fleet does not yet have an OAuth app:
/// - GitHub: land on github.com via App Manifest (create + install + authorize)
/// - others (Jira, …): one-time in-tab form, then 302 to the provider login
async fn oauth_start(
    State(state): State<IntegrationsState>,
    jar: CookieJar,
    Path((workspace_id, provider)): Path<(Uuid, String)>,
) -> AppResult<Response> {
    let (user, _) = require_workspace_member(&state, &jar, workspace_id).await?;
    let provider = provider.trim().to_lowercase();
    let entry = require_available_provider(&provider)?;

    if let Some(client) = resolve_oauth_client(&state, workspace_id, &provider).await? {
        return redirect_to_provider(&state.config, &user.id, workspace_id, &provider, &client);
    }

    // Env + workspace app unset: still start Connect (must not 500).
    oauth_start_unconfigured(&state.config, user.id, workspace_id, &provider, &entry)
}

#[derive(Debug, Deserialize)]
struct OauthAppForm {
    client_id: String,
    client_secret: String,
}

/// First-time Jira/Slack/… : save the provider OAuth app, then 302 to their login.
async fn oauth_start_with_app(
    State(state): State<IntegrationsState>,
    jar: CookieJar,
    Path((workspace_id, provider)): Path<(Uuid, String)>,
    Form(form): Form<OauthAppForm>,
) -> AppResult<Response> {
    let (user, _) = require_workspace_member(&state, &jar, workspace_id).await?;
    let provider = provider.trim().to_lowercase();
    require_available_provider(&provider)?;

    let client_id = form.client_id.trim().to_string();
    let client_secret = form.client_secret.trim().to_string();
    if client_id.is_empty() || client_secret.is_empty() {
        return Err(AppError::bad_request(
            "client_id and client_secret are required",
        ));
    }

    persist_oauth_app(
        &state,
        workspace_id,
        &provider,
        &client_id,
        serde_json::json!({ "client_secret": client_secret }),
        user.id,
    )
    .await?;

    let client = OAuthClientConfig {
        client_id,
        client_secret,
        github_app_slug: None,
    };
    redirect_to_provider(&state.config, &user.id, workspace_id, &provider, &client)
}

fn require_available_provider(provider: &str) -> AppResult<crate::models::IntegrationView> {
    let entry = find_provider(provider)
        .ok_or_else(|| AppError::not_found(format!("unknown integration: {provider}")))?;
    if !matches!(entry.availability, IntegrationAvailability::Available) {
        return Err(AppError::bad_request(format!(
            "{provider} is not available yet"
        )));
    }
    Ok(entry)
}

/// When env + workspace have no OAuth app, start Connect anyway:
/// GitHub App Manifest, or a one-time in-tab form for everyone else.
fn oauth_start_unconfigured(
    config: &crate::config::IntegrationsConfig,
    user_id: Uuid,
    workspace_id: Uuid,
    provider: &str,
    entry: &crate::models::IntegrationView,
) -> AppResult<Response> {
    let oauth_state = issue_oauth_state(&config.state_secret, user_id, workspace_id, provider)?;
    if provider == "github" {
        tracing::info!(%workspace_id, user_id = %user_id, "oauth start github manifest");
        return Ok(html_page(manifest_form_html(
            config,
            workspace_id,
            &oauth_state,
        )));
    }
    tracing::info!(
        %workspace_id,
        provider = %provider,
        user_id = %user_id,
        "oauth start provider bootstrap"
    );
    Ok(html_page(provider_bootstrap_html(
        &entry.name,
        provider,
        oauth_app_create_url(provider).unwrap_or("#"),
        &config.callback_url(provider),
        &format!("/api/workspaces/{workspace_id}/integrations/{provider}/oauth/start"),
    )))
}

fn redirect_to_provider(
    config: &crate::config::IntegrationsConfig,
    user_id: &Uuid,
    workspace_id: Uuid,
    provider: &str,
    client: &OAuthClientConfig,
) -> AppResult<Response> {
    let oauth_state = issue_oauth_state(&config.state_secret, *user_id, workspace_id, provider)?;
    let url = authorize_url(provider, config, client, &oauth_state)?;
    tracing::info!(
        %workspace_id,
        provider = %provider,
        user_id = %user_id,
        "oauth start"
    );
    Ok(Redirect::temporary(&url).into_response())
}

async fn persist_oauth_app(
    state: &IntegrationsState,
    workspace_id: Uuid,
    provider: &str,
    client_id: &str,
    secret_plain: serde_json::Value,
    user_id: Uuid,
) -> AppResult<()> {
    let sealed = state
        .config
        .credentials_key
        .encrypt(&secret_plain)
        .map_err(|e| AppError::internal(format!("encrypt oauth app secret: {e}")))?;
    integ_repo::upsert_oauth_app(
        &state.db,
        workspace_id,
        provider,
        client_id,
        &sealed,
        Some(user_id),
        Utc::now(),
    )
    .await?;
    tracing::info!(%workspace_id, provider = %provider, "oauth app saved");
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct OAuthCallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
    pub installation_id: Option<i64>,
    pub setup_action: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubManifestQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// GitHub redirects here after the user creates the App from the manifest.
/// Convert the temp code → client_id/secret, then send them to install + authorize.
///
/// `jar` is the browser session. A stolen manifest `code`+`state` URL must
/// not persist an App onto the state workspace.
async fn github_manifest_callback(
    State(state): State<IntegrationsState>,
    jar: CookieJar,
    Query(q): Query<GithubManifestQuery>,
) -> Response {
    let ui_origin = state.config.ui_public_base.clone();
    match github_manifest_callback_inner(&state, &jar, q).await {
        Ok(url) => Redirect::temporary(&url).into_response(),
        Err(err) => {
            tracing::warn!(error = %err, "github manifest callback failed");
            let mut res = oauth_popup_close_html(&ui_origin, None, Some(err.message()));
            if let Some(status) = err.status() {
                *res.status_mut() = status;
            }
            res
        }
    }
}

async fn github_manifest_callback_inner(
    state: &IntegrationsState,
    jar: &CookieJar,
    q: GithubManifestQuery,
) -> Result<String, OAuthCallbackError> {
    if let Some(err) = q.error {
        let desc = q.error_description.unwrap_or(err);
        return Err(OAuthCallbackError::client(
            None,
            format!("GitHub denied app creation: {desc}"),
        ));
    }
    let code = q
        .code
        .filter(|s| !s.is_empty())
        .ok_or_else(|| OAuthCallbackError::client(None, "missing GitHub manifest code"))?;
    let state_jwt = q
        .state
        .filter(|s| !s.is_empty())
        .ok_or_else(|| OAuthCallbackError::client(None, "missing OAuth state"))?;
    let claims = verify_oauth_state(&state.config.state_secret, &state_jwt)
        .map_err(|e| OAuthCallbackError::invalid_state(None, e.to_string()))?;
    if claims.provider != "github" {
        return Err(OAuthCallbackError::provider_mismatch(Some(
            claims.workspace_id.clone(),
        )));
    }
    let workspace_id = Uuid::parse_str(&claims.workspace_id).map_err(|_| {
        OAuthCallbackError::client(Some(claims.workspace_id.clone()), "bad workspace in state")
    })?;
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| {
        OAuthCallbackError::client(Some(claims.workspace_id.clone()), "bad user in state")
    })?;

    let member = workspace_repo::find_for_user(&state.db, workspace_id, user_id)
        .await
        .map_err(|e| OAuthCallbackError::other(Some(claims.workspace_id.clone()), e.to_string()))?;
    if member.is_none() {
        return Err(OAuthCallbackError::not_a_member(Some(
            claims.workspace_id.clone(),
        )));
    }

    // Same bind as user-oauth: fleet_access required, session user ==
    // state.sub, member of state.workspace_id. Check before convert so a
    // stolen code+state never fetches client_secret / PEM.
    bind_callback_session(state, jar, workspace_id, user_id, &claims.workspace_id).await?;

    let app = convert_manifest(&code)
        .await
        .map_err(|e| OAuthCallbackError::from_app_error(e, Some(claims.workspace_id.clone())))?;
    persist_oauth_app(
        state,
        workspace_id,
        "github",
        &app.client_id,
        serde_json::json!({
            "client_secret": app.client_secret,
            "github_app_slug": app.slug,
            "github_app_id": app.app_id,
            "webhook_secret": app.webhook_secret,
            "pem": app.pem,
        }),
        user_id,
    )
    .await
    .map_err(|e| OAuthCallbackError::from_app_error(e, Some(claims.workspace_id.clone())))?;

    tracing::info!(
        %workspace_id,
        user_id = %user_id,
        slug = %app.slug,
        "github app created from manifest"
    );

    Ok(github_install_url(&app.slug, &state_jwt))
}

/// OAuth callback: exchange code, store tokens, notify opener, close tab.
///
/// `jar` is the browser session. State `workspace_id` must match that
/// session's workspace — a Connect started on A must not complete on B.
async fn oauth_callback(
    State(state): State<IntegrationsState>,
    jar: CookieJar,
    Path(provider): Path<String>,
    Query(q): Query<OAuthCallbackQuery>,
) -> Response {
    let provider = provider.trim().to_lowercase();
    // Trello puts the user token in the URL fragment. Serve a tiny page that
    // copies `#token=` into `?code=` so the shared callback can store it.
    if provider == "trello" && q.code.is_none() && q.error.is_none() {
        return trello_token_capture_html();
    }
    let ui_origin = state.config.ui_public_base.clone();
    // SameSite=Lax sends fleet_access on a real top-level provider redirect.
    // A missing cookie is not a guest Connect — stolen state JWT + code
    // must not exchange or upsert. Reject before inner so the nonce is
    // not consumed.
    if jar.get(fleet_auth::ACCESS_COOKIE).is_none() {
        tracing::warn!(
            provider = %provider,
            "oauth callback rejected: no session"
        );
        let mut res = oauth_popup_close_html(&ui_origin, None, Some("not authenticated"));
        *res.status_mut() = StatusCode::UNAUTHORIZED;
        return res;
    }
    match oauth_callback_inner(&state, &provider, q, &jar).await {
        Ok(provider_id) => oauth_popup_close_html(&ui_origin, Some(&provider_id), None),
        Err(err) => {
            // User denials / missing params are expected; everything else is unexpected.
            if err.is_expected() {
                tracing::warn!(
                    provider = %provider,
                    workspace = ?err.workspace_hint,
                    error = %err,
                    "oauth callback rejected"
                );
            } else {
                tracing::error!(
                    provider = %provider,
                    workspace = ?err.workspace_hint,
                    error = %err,
                    "oauth callback failed"
                );
            }
            let mut res = oauth_popup_close_html(&ui_origin, None, Some(err.message()));
            if let Some(status) = err.status() {
                *res.status_mut() = status;
            }
            res
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

fn trello_token_capture_html() -> Response {
    let html = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <title>Connecting Trello</title>
</head>
<body>
<p>Finishing Trello authorization…</p>
<script>
(function () {
  var h = new URLSearchParams((location.hash || "").replace(/^#/, ""));
  var token = h.get("token") || h.get("access_token");
  var u = new URL(location.href);
  u.hash = "";
  if (token) {
    u.searchParams.set("code", token);
  } else {
    u.searchParams.set("error", "access_denied");
    u.searchParams.set("error_description", "Trello did not return a token");
  }
  location.replace(u.toString());
})();
</script>
</body>
</html>"#;
    html_page(html.to_string())
}

fn html_page(html: String) -> Response {
    let mut res = Html(html).into_response();
    *res.status_mut() = StatusCode::OK;
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    res.headers_mut().insert(
        HeaderName::from_static("cross-origin-opener-policy"),
        HeaderValue::from_static("unsafe-none"),
    );
    res
}

/// One-time in-tab page when the provider has no Fleet OAuth app yet (Jira, Slack, …).
/// Submit → POST oauth/start → 302 to provider login + consent.
fn provider_bootstrap_html(
    name: &str,
    provider: &str,
    create_url: &str,
    callback_url: &str,
    action: &str,
) -> String {
    let scopes = match provider {
        "jira" => {
            "In the Atlassian app, add callback URL below and scopes: \
             <code>read:jira-work</code> <code>write:jira-work</code> \
             <code>read:jira-user</code> <code>offline_access</code>."
        }
        "slack" => "Create a Slack app with OAuth redirect URL below.",
        "linear" => "Create a Linear OAuth app with callback URL below.",
        _ => {
            "Create an OAuth app at the provider, paste the client id and secret, then continue to sign in."
        }
    };
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Connect {name}</title>
  <style>
    :root {{ color-scheme: light; }}
    body {{ font: 15px/1.45 system-ui, sans-serif; margin: 0; background: #f4f6fa; color: #111; }}
    main {{ max-width: 28rem; margin: 2.5rem auto; background: #fff; border: 1px solid #d8dee8;
            border-radius: 12px; padding: 1.25rem 1.35rem; }}
    h1 {{ font-size: 1.15rem; margin: 0 0 0.5rem; }}
    p, label {{ color: #3c4553; }}
    code, .cb {{ font: 12px/1.4 ui-monospace, monospace; word-break: break-all; }}
    .cb {{ display: block; background: #f0f3f8; border-radius: 8px; padding: 0.55rem 0.7rem; }}
    input {{ width: 100%; box-sizing: border-box; margin: 0.25rem 0 0.75rem; padding: 0.5rem 0.6rem;
             border: 1px solid #c5cedb; border-radius: 8px; font: inherit; }}
    button {{ font: inherit; font-weight: 600; background: #0b6bcb; color: #fff; border: 0;
              border-radius: 8px; padding: 0.55rem 0.9rem; cursor: pointer; width: 100%; }}
    a {{ color: #0b6bcb; }}
  </style>
</head>
<body>
  <main>
    <h1>Sign in with {name}</h1>
    <p>{provider} sends you to their login next. They require an OAuth app first
       (one-time). Create it, then you will grant Agent Fleet permissions on your account.</p>
    <p><a href="{create_url}" target="_blank" rel="noreferrer">Create a {name} OAuth app</a></p>
    <p>{scopes}</p>
    <label>Authorization callback URL</label>
    <span class="cb">{callback}</span>
    <form method="post" action="{action}" autocomplete="off">
      <label for="client_id">Client ID</label>
      <input id="client_id" name="client_id" required>
      <label for="client_secret">Client secret</label>
      <input id="client_secret" name="client_secret" type="password" required>
      <button type="submit">Continue to {name}</button>
    </form>
  </main>
</body>
</html>"#,
        name = html_escape(name),
        provider = html_escape(name),
        create_url = html_escape(create_url),
        scopes = scopes,
        callback = html_escape(callback_url),
        action = html_escape(action),
    )
}

/// Session cookie must be present, user must match `state.sub`, and they
/// must still be a member of `state.workspace_id`.
async fn bind_callback_session(
    state: &IntegrationsState,
    jar: &CookieJar,
    workspace_id: Uuid,
    state_user_id: Uuid,
    workspace_hint: &str,
) -> Result<(), OAuthCallbackError> {
    let hint = Some(workspace_hint.to_string());
    match require_workspace_member(state, jar, workspace_id).await {
        Ok((session_user, _)) => {
            if session_user.id != state_user_id {
                return Err(OAuthCallbackError::user_mismatch(hint));
            }
            Ok(())
        }
        Err(e) => Err(OAuthCallbackError::from_app_error(e, hint)),
    }
}

/// Consume the state nonce, exchange the code, and upsert the connection.
pub async fn oauth_callback_inner(
    state: &IntegrationsState,
    provider: &str,
    q: OAuthCallbackQuery,
    jar: &CookieJar,
) -> Result<String, OAuthCallbackError> {
    let provider = provider.trim().to_lowercase();

    if let Some(err) = q.error {
        let desc = q.error_description.unwrap_or(err);
        return Err(OAuthCallbackError::client(
            None,
            format!("provider denied access: {desc}"),
        ));
    }

    let code = q
        .code
        .filter(|s| !s.is_empty())
        .ok_or_else(|| OAuthCallbackError::client(None, "missing OAuth code"))?;
    let state_jwt = q
        .state
        .filter(|s| !s.is_empty())
        .ok_or_else(|| OAuthCallbackError::client(None, "missing OAuth state"))?;

    let claims = verify_oauth_state(&state.config.state_secret, &state_jwt)
        .map_err(|e| OAuthCallbackError::invalid_state(None, e.to_string()))?;

    if claims.provider != provider {
        return Err(OAuthCallbackError::provider_mismatch(Some(
            claims.workspace_id.clone(),
        )));
    }

    let workspace_id = Uuid::parse_str(&claims.workspace_id).map_err(|_| {
        OAuthCallbackError::client(Some(claims.workspace_id.clone()), "bad workspace in state")
    })?;
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| {
        OAuthCallbackError::client(Some(claims.workspace_id.clone()), "bad user in state")
    })?;

    // Ensure user still a member (state alone is not enough if membership revoked).
    let member = workspace_repo::find_for_user(&state.db, workspace_id, user_id)
        .await
        .map_err(|e| OAuthCallbackError::other(Some(claims.workspace_id.clone()), e.to_string()))?;
    if member.is_none() {
        return Err(OAuthCallbackError::not_a_member(Some(
            claims.workspace_id.clone(),
        )));
    }

    // Browser session is required. SameSite=Lax sends fleet_access on a
    // real top-level provider redirect; a missing cookie is not a guest
    // Connect. Session user must equal state.sub and belong to the state
    // workspace — a Connect started on A completed with B's cookie is 403.
    bind_callback_session(state, jar, workspace_id, user_id, &claims.workspace_id).await?;

    let client = resolve_oauth_client(state, workspace_id, &provider)
        .await
        .map_err(|e| OAuthCallbackError::from_app_error(e, Some(claims.workspace_id.clone())))?
        .ok_or_else(|| {
            OAuthCallbackError::client(
                Some(claims.workspace_id.clone()),
                format!("{provider} OAuth app is not set"),
            )
        })?;

    // Consume before the token exchange so a replay or a parallel callback
    // cannot attach or replace the connection. Verify-only (manifest) does
    // not call this.
    let consumed =
        integ_repo::consume_oauth_state_nonce(&state.db, &claims.nonce, workspace_id, &provider)
            .await
            .map_err(|e| {
                OAuthCallbackError::other(Some(claims.workspace_id.clone()), e.to_string())
            })?;
    if !consumed {
        return Err(OAuthCallbackError::nonce_reuse(Some(
            claims.workspace_id.clone(),
        )));
    }

    let tokens = exchange_code(&provider, &state.config, &client, &code)
        .await
        .map_err(|e| OAuthCallbackError::from_app_error(e, Some(claims.workspace_id.clone())))?;

    let mut config = tokens.config;
    if let Some(installation_id) = q.installation_id {
        config
            .as_object_mut()
            .map(|o| o.insert("installation_id".into(), serde_json::json!(installation_id)));
    }
    if let Some(setup_action) = q.setup_action.filter(|s| !s.is_empty()) {
        config
            .as_object_mut()
            .map(|o| o.insert("setup_action".into(), serde_json::json!(setup_action)));
    }

    // Encrypt tokens before they hit Postgres (AES-256-GCM envelope).
    let sealed = state
        .config
        .credentials_key
        .encrypt(&tokens.credentials)
        .map_err(|e| {
            OAuthCallbackError::other(
                Some(claims.workspace_id.clone()),
                format!("failed to encrypt credentials: {e}"),
            )
        })?;

    let now = Utc::now();
    integ_repo::upsert(
        &state.db,
        &UpsertConnection {
            id: Uuid::now_v7(),
            workspace_id,
            provider: provider.clone(),
            account_label: tokens.account_label,
            config,
            credentials: sealed,
            connected_by: Some(user_id),
            at: now,
        },
    )
    .await
    .map_err(|e| OAuthCallbackError::other(Some(claims.workspace_id.clone()), e.to_string()))?;

    tracing::info!(
        %workspace_id,
        provider = %provider,
        user_id = %user_id,
        "oauth connected"
    );

    Ok(provider)
}

#[derive(Debug, Deserialize)]
struct PatchDefaultsBody {
    #[serde(default)]
    default_runner_id: Option<String>,
    #[serde(default)]
    default_provider: Option<String>,
    #[serde(default)]
    default_model: Option<String>,
}

/// `PATCH /api/workspaces/:id/integrations/:provider` — agent defaults.
async fn patch_integration_defaults(
    State(state): State<IntegrationsState>,
    jar: CookieJar,
    Path((workspace_id, provider)): Path<(Uuid, String)>,
    Json(body): Json<PatchDefaultsBody>,
) -> AppResult<Json<IntegrationConnectionPublic>> {
    let _ = require_workspace_member(&state, &jar, workspace_id).await?;
    let provider = provider.trim().to_lowercase();
    if !catalog::is_known_provider(&provider) {
        return Err(AppError::not_found(format!(
            "unknown integration: {provider}"
        )));
    }
    let runner = body
        .default_runner_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let llm = body
        .default_provider
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let model = body
        .default_model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let row =
        integ_repo::patch_agent_defaults(&state.db, workspace_id, &provider, runner, llm, model)
            .await?;
    Ok(Json(IntegrationConnectionPublic::from(row)))
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
        return Err(AppError::not_found(format!(
            "unknown integration: {provider}"
        )));
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

#[cfg(test)]
mod patch_agent_defaults_tests {
    use super::*;
    use fleet_db::{IntegrationAgentDefaults, IntegrationConnection, IntegrationConnectionPublic};
    use serde_json::json;

    /// PATCH response is IntegrationConnectionPublic: defaults from config, no secrets.
    #[test]
    fn patch_agent_defaults_only_updates_connected_never_inserts() {
        let now = Utc::now();
        let row = IntegrationConnection {
            id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            provider: "github".into(),
            status: "connected".into(),
            account_label: Some("acct".into()),
            config: json!({
                "default_runner_id": "runner-1",
                "default_provider": "grok",
                "default_model": "model-1",
                "site": "github.com",
            }),
            credentials: json!({
                "_enc": "v1",
                "ct": "deadbeef",
                "access_token": "tok_must_not_serialize",
                "refresh_token": "ref_must_not_serialize",
                "client_secret": "secret_must_not_serialize",
            }),
            connected_by: None,
            last_error: None,
            connected_at: now,
            updated_at: now,
            version: 2,
        };
        let public = IntegrationConnectionPublic::from(&row);
        assert_eq!(
            public.defaults,
            IntegrationAgentDefaults {
                default_runner_id: Some("runner-1".into()),
                default_provider: Some("grok".into()),
                default_model: Some("model-1".into()),
            }
        );
        assert!(public.connected);
        let body = serde_json::to_value(&public).expect("serialize public");
        let obj = body.as_object().expect("object");
        assert!(
            !obj.contains_key("credentials"),
            "public envelope must omit credentials: {body}"
        );
        let hay = body.to_string();
        for needle in [
            "access_token",
            "refresh_token",
            "client_secret",
            "tok_must_not_serialize",
            "ref_must_not_serialize",
            "secret_must_not_serialize",
            "deadbeef",
        ] {
            assert!(
                !hay.contains(needle),
                "public PATCH body must not contain {needle}: {body}"
            );
        }
    }
}

#[cfg(test)]
mod oauth_start_unconfigured_tests {
    use super::*;
    use crate::config::IntegrationsConfig;
    use crate::crypto::CredentialsKey;
    use axum::body::to_bytes;

    fn unconfigured_config() -> IntegrationsConfig {
        IntegrationsConfig {
            api_public_base: "http://localhost:8080".into(),
            ui_public_base: "http://localhost:3030".into(),
            state_secret: "dev-change-me-to-a-long-random-secret-at-least-32-chars".into(),
            credentials_key: CredentialsKey::from_secret(
                "dev-change-me-to-a-long-random-secret-at-least-32-chars",
            )
            .expect("key"),
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
            trello: None,
            asana: None,
            bitbucket: None,
            confluence: None,
        }
    }

    async fn html_of(res: Response) -> (StatusCode, String, String) {
        let status = res.status();
        let ct = res
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = to_bytes(res.into_body(), 1024 * 1024)
            .await
            .expect("read body");
        (status, ct, String::from_utf8_lossy(&bytes).into_owned())
    }

    fn available_or_client_error(id: &str) -> Option<crate::models::IntegrationView> {
        match require_available_provider(id) {
            Ok(entry) => Some(entry),
            Err(AppError::BadRequest(_) | AppError::NotFound(_)) => None,
            Err(other) => panic!("unconfigured start gate must not 500 for {id}: {other:?}"),
        }
    }

    async fn unconfigured_html(
        cfg: &IntegrationsConfig,
        user_id: Uuid,
        workspace_id: Uuid,
        entry: &crate::models::IntegrationView,
    ) -> String {
        let res = oauth_start_unconfigured(cfg, user_id, workspace_id, &entry.id, entry)
            .unwrap_or_else(|e| {
                panic!(
                    "available {} with unset client must not 500: {e:?}",
                    entry.id
                )
            });
        assert!(
            !res.status().is_server_error(),
            "available {} unconfigured start must not 5xx, got {}",
            entry.id,
            res.status()
        );
        let (status, ct, body) = html_of(res).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "available {} unconfigured start is bootstrap HTML, not 302/4xx: {}",
            entry.id,
            status
        );
        assert!(
            ct.contains("text/html"),
            "{} expected HTML, content-type={ct:?} body={body}",
            entry.id
        );
        body
    }

    /// Docs: if client id/secret unset, Connect still starts. Must not 500.
    #[tokio::test]
    async fn oauth_start_unconfigured_still_bootstraps() {
        let cfg = unconfigured_config();
        let user_id = Uuid::now_v7();
        let workspace_id = Uuid::now_v7();
        let mut saw_available = false;
        for view in catalog::catalog() {
            let Some(entry) = available_or_client_error(&view.id) else {
                continue;
            };
            saw_available = true;
            let _body = unconfigured_html(&cfg, user_id, workspace_id, &entry).await;
        }
        assert!(
            saw_available,
            "catalog must include at least one Available provider"
        );
    }

    #[tokio::test]
    async fn oauth_start_unconfigured_form_posts_to_oauth_start_and_shows_callback() {
        let cfg = unconfigured_config();
        let user_id = Uuid::now_v7();
        let workspace_id = Uuid::now_v7();
        let mut saw_other = false;
        for view in catalog::catalog() {
            let Some(entry) = available_or_client_error(&view.id) else {
                continue;
            };
            if entry.id == "github" {
                continue;
            }
            saw_other = true;
            let body = unconfigured_html(&cfg, user_id, workspace_id, &entry).await;
            let action = format!(
                "/api/workspaces/{workspace_id}/integrations/{}/oauth/start",
                entry.id
            );
            assert!(
                body.contains(&action),
                "{} bootstrap form must POST back to oauth/start: {body}",
                entry.id
            );
            assert!(
                body.contains(&cfg.callback_url(&entry.id)),
                "{} form must show API_PUBLIC_BASE callback: {body}",
                entry.id
            );
        }
        assert!(
            saw_other,
            "need at least one non-github Available provider form"
        );
    }

    #[tokio::test]
    async fn oauth_start_configured_github_redirects_to_user_oauth_not_manifest() {
        let user_id = Uuid::now_v7();
        let workspace_id = Uuid::now_v7();
        let mut configured = unconfigured_config();
        configured.github = Some(OAuthClientConfig {
            client_id: "Iv1.configured-client".into(),
            client_secret: "configured-secret".into(),
            github_app_slug: None,
        });
        assert!(configured.is_oauth_configured("github"));
        let client = configured.github.clone().expect("github client just set");
        let redirect = redirect_to_provider(&configured, &user_id, workspace_id, "github", &client)
            .expect("configured start redirects");
        assert!(
            redirect.status().is_redirection(),
            "configured github must 302, got {}",
            redirect.status()
        );
        let loc = redirect
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(
            loc.contains("github.com/login/oauth/authorize"),
            "configured github Location must be user-oauth authorize, got {loc}"
        );
        assert!(
            !loc.contains("/settings/apps/new"),
            "configured github must not use App Manifest: {loc}"
        );
    }

    #[tokio::test]
    async fn oauth_start_unconfigured_html_embeds_oauth_app_create_url() {
        let cfg = unconfigured_config();
        let user_id = Uuid::now_v7();
        let workspace_id = Uuid::now_v7();
        let mut saw_other = false;
        for view in catalog::catalog() {
            let Some(entry) = available_or_client_error(&view.id) else {
                continue;
            };
            if entry.id == "github" {
                continue;
            }
            saw_other = true;
            let body = unconfigured_html(&cfg, user_id, workspace_id, &entry).await;
            let create = oauth_app_create_url(&entry.id).unwrap_or_else(|| {
                panic!(
                    "{} bootstrap must not fall back to leftover #; missing oauth_app_create_url",
                    entry.id
                )
            });
            assert_ne!(
                create, "#",
                "{} create URL must not be leftover #",
                entry.id
            );
            assert!(
                body.contains(create),
                "{} bootstrap HTML must embed oauth_app_create_url ({create}): {body}",
                entry.id
            );
        }
        assert!(
            saw_other,
            "need at least one non-github Available provider form"
        );
    }

    #[tokio::test]
    async fn oauth_start_unconfigured_form_method_is_post() {
        let cfg = unconfigured_config();
        let user_id = Uuid::now_v7();
        let workspace_id = Uuid::now_v7();
        let mut saw_other = false;
        for view in catalog::catalog() {
            let Some(entry) = available_or_client_error(&view.id) else {
                continue;
            };
            if entry.id == "github" {
                continue;
            }
            saw_other = true;
            let body = unconfigured_html(&cfg, user_id, workspace_id, &entry).await;
            assert!(
                body.contains("method=\"post\""),
                "{} bootstrap form must POST (GET would leak client_secret): {body}",
                entry.id
            );
        }
        assert!(
            saw_other,
            "need at least one non-github Available provider form"
        );
    }

    #[tokio::test]
    async fn oauth_start_unconfigured_github_action_includes_issued_state() {
        let cfg = unconfigured_config();
        let user_id = Uuid::now_v7();
        let workspace_id = Uuid::now_v7();
        let entry = require_available_provider("github").expect("github available");
        let body = unconfigured_html(&cfg, user_id, workspace_id, &entry).await;
        let marker = "/settings/apps/new?state=";
        assert!(
            body.contains(marker),
            "GitHub unconfigured action must include issued state: {body}"
        );
        let start = body.find(marker).expect("action state") + marker.len();
        let end = body[start..]
            .find('"')
            .unwrap_or_else(|| panic!("state close quote: {body}"));
        let state = &body[start..start + end];
        let claims = verify_oauth_state(&cfg.state_secret, state)
            .expect("form action state is the issued JWT");
        assert_eq!(claims.typ, "oauth_state");
        assert_eq!(claims.sub, user_id.to_string());
        assert_eq!(claims.workspace_id, workspace_id.to_string());
        assert_eq!(claims.provider, "github");
    }
}

#[cfg(test)]
mod token_paste_tests {
    use super::*;
    use crate::config::IntegrationsConfig;
    use crate::crypto::CredentialsKey;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt;

    fn paste_config() -> IntegrationsConfig {
        IntegrationsConfig {
            api_public_base: "http://localhost:8080".into(),
            ui_public_base: "http://localhost:3030".into(),
            state_secret: "token-paste-not-accepted-secret-32ch!".into(),
            credentials_key: CredentialsKey::from_secret("token-paste-not-accepted-secret-32ch!")
                .expect("key"),
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
            trello: None,
            asana: None,
            bitbucket: None,
            confluence: None,
        }
    }

    fn paste_state() -> IntegrationsState {
        let pool = fleet_db::PgPool::connect_lazy("postgres://unused:unused@127.0.0.1:9/none")
            .expect("lazy pool");
        IntegrationsState {
            db: pool,
            auth: fleet_auth::AuthConfig {
                jwt_secret: "token-paste-not-accepted-secret-32ch!".into(),
                access_ttl: std::time::Duration::from_secs(900),
                refresh_ttl: std::time::Duration::from_secs(604_800),
                cookie_secure: false,
                cookie_samesite: axum_extra::extract::cookie::SameSite::Lax,
                signup_token: None,
                instance_admin_emails: Vec::new(),
            },
            config: paste_config(),
        }
    }

    async fn post_body(
        app: &axum::Router,
        path: &str,
        content_type: &str,
        body: String,
    ) -> (StatusCode, String) {
        let req = Request::builder()
            .method("POST")
            .uri(path)
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(body))
            .expect("token-paste request");
        let res = app.clone().oneshot(req).await.expect("token-paste oneshot");
        let status = res.status();
        let bytes = to_bytes(res.into_body(), 1024 * 1024)
            .await
            .expect("read body");
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn get_path(app: &axum::Router, path: &str) -> (StatusCode, String, String) {
        let req = Request::builder()
            .method("GET")
            .uri(path)
            .body(Body::empty())
            .expect("get request");
        let res = app.clone().oneshot(req).await.expect("get oneshot");
        let status = res.status();
        let ct = res
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = to_bytes(res.into_body(), 1024 * 1024)
            .await
            .expect("read body");
        (status, ct, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Docs: no personal access token paste. Connect is OAuth consent only.
    /// router() registers no …/token POST; a pasted PAT is 404 and not echoed.
    #[tokio::test]
    async fn token_paste_post_is_not_found_and_does_not_echo_pat() {
        let state = paste_state();
        state.db.close().await;
        let app = router().with_state(state);
        let ws = Uuid::now_v7();
        let pat = format!("ghp_must_not_accept_{}", Uuid::now_v7().as_simple());
        let path = format!("/api/workspaces/{ws}/integrations/github/token");
        let (status, body) = post_body(
            &app,
            &path,
            "application/json",
            serde_json::json!({ "token": pat, "pat": pat, "api_key": pat }).to_string(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "unregistered token POST must be 404, got {status} body={body}"
        );
        assert!(
            !body.contains(&pat),
            "reject body must not echo the pasted token: {body}"
        );
    }

    /// Trello's documented flow is the authorize fragment (`#token=` → `?code=`),
    /// not a PAT/API-key paste form.
    #[tokio::test]
    async fn trello_callback_without_code_is_fragment_capture_not_a_paste_form() {
        let state = paste_state();
        state.db.close().await;
        let app = router().with_state(state);
        let (tr_status, tr_ct, tr_body) =
            get_path(&app, "/api/integrations/oauth/callback/trello").await;
        assert_eq!(
            tr_status,
            StatusCode::OK,
            "Trello callback without code is the fragment capture page, got {tr_status} {tr_body}"
        );
        assert!(
            tr_ct.contains("text/html"),
            "Trello capture must be HTML, content-type={tr_ct:?} body={tr_body}"
        );
        let tr_lower = tr_body.to_ascii_lowercase();
        assert!(
            tr_body.contains("location.hash")
                && (tr_body.contains("h.get(\"token\")") || tr_body.contains("h.get('token')"))
                && tr_body.contains("code"),
            "Trello must lift #token= into ?code= (OAuth user-token), got {tr_body}"
        );
        assert!(
            !tr_lower.contains("<input")
                && !tr_lower.contains("<textarea")
                && !tr_lower.contains("name=\"token\"")
                && !tr_lower.contains("name=\"api_key\"")
                && !tr_lower.contains("name=\"access_token\"")
                && !tr_lower.contains("name=\"pat\"")
                && !tr_lower.contains("personal access token")
                && !tr_lower.contains("paste"),
            "Trello user-token flow must not be a PAT/API-key paste form: {tr_body}"
        );
    }

    #[test]
    fn trello_authorize_url_uses_fragment_token() {
        let cfg = paste_config();
        let client = OAuthClientConfig {
            client_id: "trello-key".into(),
            client_secret: "trello-secret".into(),
            github_app_slug: None,
        };
        let url =
            authorize_url("trello", &cfg, &client, "state-token").expect("trello authorize_url");
        assert!(
            url.contains("response_type=token"),
            "Trello authorize must request a fragment token, got {url}"
        );
        assert!(
            url.contains("callback_method=fragment"),
            "Trello authorize must return the token in the fragment, got {url}"
        );
    }

    #[tokio::test]
    async fn oauth_start_with_app_ignores_pasted_user_token() {
        let state = paste_state();
        state.db.close().await;
        let app = router().with_state(state);
        let ws = Uuid::now_v7();
        let pat = format!("ghp_must_not_persist_{}", Uuid::now_v7().as_simple());
        let path = format!("/api/workspaces/{ws}/integrations/github/oauth/start");
        let body = format!(
            "client_id=iv1.app&client_secret=s3cret&token={pat}&pat={pat}&access_token={pat}"
        );
        let (status, resp) =
            post_body(&app, &path, "application/x-www-form-urlencoded", body).await;
        assert_ne!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "OauthAppForm has no token field; extra token/pat must be ignored, got {status} {resp}"
        );
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "without a session, start must not persist, got {status} {resp}"
        );
        assert!(
            !resp.contains(&pat),
            "start must not echo a pasted user token: {resp}"
        );
    }

    #[tokio::test]
    async fn trello_token_capture_reads_access_token_alias() {
        let state = paste_state();
        state.db.close().await;
        let app = router().with_state(state);
        let (_, _, tr_body) = get_path(&app, "/api/integrations/oauth/callback/trello").await;
        assert!(
            tr_body.contains("h.get(\"access_token\")"),
            "Trello capture must also lift #access_token=: {tr_body}"
        );
    }
}
