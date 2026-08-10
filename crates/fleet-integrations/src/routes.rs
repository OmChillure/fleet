//! List / OAuth start / OAuth callback / disconnect.

use axum::Json;
use axum::extract::{Form, FromRef, Path, Query, State};
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

use crate::catalog::{self, find_provider, oauth_app_create_url};
use crate::config::OAuthClientConfig;
use crate::models::{
    DisconnectResponse, IntegrationAvailability, IntegrationListResponse, IntegrationView,
};
use crate::oauth::{
    authorize_url, convert_manifest, exchange_code, github_install_url, issue_oauth_state,
    manifest_form_html, verify_oauth_state,
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
            "/api/integrations/webhooks/github",
            get(github_webhook_stub).post(github_webhook_stub),
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
    workspace_app_providers: &std::collections::HashSet<String>,
    state: &IntegrationsState,
) -> Vec<IntegrationView> {
    let by_provider: std::collections::HashMap<&str, &fleet_db::IntegrationConnection> =
        connections.iter().map(|c| (c.provider.as_str(), c)).collect();

    catalog::catalog()
        .into_iter()
        .map(|mut view| {
            let configured = state.config.is_oauth_configured(&view.id)
                || workspace_app_providers.contains(&view.id);
            view.oauth_configured = configured;
            view.oauth_callback_url = Some(state.config.callback_url(&view.id));
            view.oauth_app_create_url = oauth_app_create_url(&view.id).map(str::to_string);
            if matches!(view.availability, IntegrationAvailability::Available) {
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
    let (user, _) = require_workspace_member(&state, &jar, workspace_id).await?;
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
        return Err(AppError::bad_request("client_id and client_secret are required"));
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
        return redirect_to_provider(&state, &user.id, workspace_id, &provider, &client);
    }

    let oauth_state = issue_oauth_state(
        &state.config.state_secret,
        user.id,
        workspace_id,
        &provider,
    )?;

    if provider == "github" {
        tracing::info!(%workspace_id, user_id = %user.id, "oauth start github manifest");
        return Ok(html_page(manifest_form_html(
            &state.config,
            workspace_id,
            &oauth_state,
        )));
    }

    tracing::info!(
        %workspace_id,
        provider = %provider,
        user_id = %user.id,
        "oauth start provider bootstrap"
    );
    Ok(html_page(provider_bootstrap_html(
        &entry.name,
        &provider,
        oauth_app_create_url(&provider).unwrap_or("#"),
        &state.config.callback_url(&provider),
        &format!(
            "/api/workspaces/{workspace_id}/integrations/{provider}/oauth/start"
        ),
    )))
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
        return Err(AppError::bad_request("client_id and client_secret are required"));
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
    redirect_to_provider(&state, &user.id, workspace_id, &provider, &client)
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

fn redirect_to_provider(
    state: &IntegrationsState,
    user_id: &Uuid,
    workspace_id: Uuid,
    provider: &str,
    client: &OAuthClientConfig,
) -> AppResult<Response> {
    let oauth_state = issue_oauth_state(
        &state.config.state_secret,
        *user_id,
        workspace_id,
        provider,
    )?;
    let url = authorize_url(provider, &state.config, client, &oauth_state)?;
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
struct OAuthCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
    installation_id: Option<i64>,
    setup_action: Option<String>,
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
async fn github_manifest_callback(
    State(state): State<IntegrationsState>,
    Query(q): Query<GithubManifestQuery>,
) -> Response {
    let ui_origin = state.config.ui_public_base.clone();
    match github_manifest_callback_inner(&state, q).await {
        Ok(url) => Redirect::temporary(&url).into_response(),
        Err(msg) => {
            tracing::warn!(error = %msg, "github manifest callback failed");
            oauth_popup_close_html(&ui_origin, None, Some(&msg))
        }
    }
}

async fn github_manifest_callback_inner(
    state: &IntegrationsState,
    q: GithubManifestQuery,
) -> Result<String, String> {
    if let Some(err) = q.error {
        let desc = q.error_description.unwrap_or(err);
        return Err(format!("GitHub denied app creation: {desc}"));
    }
    let code = q
        .code
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "missing GitHub manifest code".to_string())?;
    let state_jwt = q
        .state
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "missing OAuth state".to_string())?;
    let claims = verify_oauth_state(&state.config.state_secret, &state_jwt)
        .map_err(|e| e.to_string())?;
    if claims.provider != "github" {
        return Err("OAuth state provider mismatch".into());
    }
    let workspace_id = Uuid::parse_str(&claims.workspace_id)
        .map_err(|_| "bad workspace in state".to_string())?;
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| "bad user in state".to_string())?;

    let member = workspace_repo::find_for_user(&state.db, workspace_id, user_id)
        .await
        .map_err(|e| e.to_string())?;
    if member.is_none() {
        return Err("not a member of this workspace".into());
    }

    let app = convert_manifest(&code).await.map_err(|e| e.to_string())?;
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
    .map_err(|e| e.to_string())?;

    tracing::info!(
        %workspace_id,
        user_id = %user_id,
        slug = %app.slug,
        "github app created from manifest"
    );

    Ok(github_install_url(&app.slug, &state_jwt))
}

async fn github_webhook_stub() -> StatusCode {
    StatusCode::NO_CONTENT
}

/// OAuth callback: exchange code, store tokens, notify opener, close tab.
async fn oauth_callback(
    State(state): State<IntegrationsState>,
    Path(provider): Path<String>,
    Query(q): Query<OAuthCallbackQuery>,
) -> Response {
    let ui_origin = state.config.ui_public_base.clone();
    match oauth_callback_inner(&state, &provider, q).await {
        Ok(provider_id) => oauth_popup_close_html(&ui_origin, Some(&provider_id), None),
        Err((workspace_hint, msg)) => {
            // User denials / missing params are expected; everything else is unexpected.
            let clientish = msg.contains("denied")
                || msg.contains("missing OAuth")
                || msg.contains("not a member")
                || msg.contains("not configured");
            if clientish {
                tracing::warn!(
                    provider = %provider,
                    workspace = ?workspace_hint,
                    error = %msg,
                    "oauth callback rejected"
                );
            } else {
                tracing::error!(
                    provider = %provider,
                    workspace = ?workspace_hint,
                    error = %msg,
                    "oauth callback failed"
                );
            }
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
        "jira" => "In the Atlassian app, add callback URL below and scopes: \
             <code>read:jira-work</code> <code>write:jira-work</code> \
             <code>read:jira-user</code> <code>offline_access</code>.",
        "slack" => "Create a Slack app with OAuth redirect URL below.",
        "linear" => "Create a Linear OAuth app with callback URL below.",
        _ => "Create an OAuth app at the provider, paste the client id and secret, then continue to sign in.",
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

    let client = resolve_oauth_client(state, workspace_id, &provider)
        .await
        .map_err(|e| (Some(claims.workspace_id.clone()), e.to_string()))?
        .ok_or_else(|| {
            (
                Some(claims.workspace_id.clone()),
                format!("{provider} OAuth app is not set"),
            )
        })?;

    let tokens = exchange_code(&provider, &state.config, &client, &code)
        .await
        .map_err(|e| (Some(claims.workspace_id.clone()), e.to_string()))?;

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
            (
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


