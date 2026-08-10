//! Browser client for fleet-server auth + workspaces (cookies via credentials: include).
//!
//! Dev: call same-origin `/api/*` so `dx serve` can proxy to the backend and cookies
//! stay first-party (reload-safe). Production: set `FLEET_AUTH_URL` at build time.

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// API origin for browser requests.
///
/// Priority:
/// 1. `FLEET_AUTH_URL` compile-time override (production split deploy)
/// 2. Browser page origin (dev + same-origin proxy via `Dioxus.toml`)
/// 3. `http://localhost:8080` (non-wasm / fallback)
pub fn auth_base_url() -> String {
    if let Some(url) = option_env!("FLEET_AUTH_URL") {
        return url.trim_end_matches('/').to_string();
    }
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(origin) = web_sys::window().and_then(|w| w.location().origin().ok()) {
            return origin;
        }
    }
    "http://localhost:8080".to_string()
}

fn api_url(path: &str) -> String {
    format!(
        "{}/{}",
        auth_base_url().trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[derive(Debug, Clone, Serialize)]
pub struct SignupBody {
    pub email: String,
    pub password: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoginBody {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UserPublic {
    pub id: String,
    pub email: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WorkspacePublic {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub role: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthResponse {
    pub user: UserPublic,
    pub message: String,
    #[serde(default)]
    pub default_workspace: Option<WorkspacePublic>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ErrorBody {
    pub error: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CreateWorkspaceBody {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
}

#[derive(Debug, Clone)]
pub enum AuthError {
    /// 401 — no session / expired access and refresh failed.
    Unauthorized(String),
    Message(String),
    Network(String),
}

impl AuthError {
    pub fn message(&self) -> String {
        match self {
            AuthError::Unauthorized(m) | AuthError::Message(m) | AuthError::Network(m) => m.clone(),
        }
    }

    pub fn is_unauthorized(&self) -> bool {
        matches!(self, AuthError::Unauthorized(_))
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

/// Attach credentials so httpOnly auth cookies are sent/stored.
#[allow(unused_mut)]
fn with_credentials(mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    #[cfg(target_arch = "wasm32")]
    {
        req = req.fetch_credentials_include();
    }
    req
}

async fn parse_json_or_error<T: for<'de> Deserialize<'de>>(
    res: reqwest::Response,
) -> Result<T, AuthError> {
    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| AuthError::Network(e.to_string()))?;

    if status.is_success() {
        serde_json::from_str(&text).map_err(|e| {
            AuthError::Network(format!("invalid success response: {e} ({text})"))
        })
    } else if status.as_u16() == 401 {
        let msg = serde_json::from_str::<ErrorBody>(&text)
            .map(|b| b.error)
            .unwrap_or_else(|_| {
                if text.is_empty() {
                    "not authenticated".into()
                } else {
                    text
                }
            });
        Err(AuthError::Unauthorized(msg))
    } else {
        let msg = serde_json::from_str::<ErrorBody>(&text)
            .map(|b| b.error)
            .unwrap_or_else(|_| {
                if text.is_empty() {
                    format!("request failed ({status})")
                } else {
                    text
                }
            });
        Err(AuthError::Message(msg))
    }
}

/// Rotate access (+ refresh) cookies using `fleet_refresh`.
pub async fn refresh_session() -> Result<AuthResponse, AuthError> {
    let res = with_credentials(client().post(api_url("/api/auth/refresh")))
        .send()
        .await
        .map_err(|e| AuthError::Network(format!("could not reach auth service: {e}")))?;
    parse_json_or_error(res).await
}

/// GET with credentials; on 401 try one silent refresh then retry once.
async fn authorized_get(path: &str) -> Result<reqwest::Response, AuthError> {
    let first = with_credentials(client().get(api_url(path)))
        .send()
        .await
        .map_err(|e| AuthError::Network(format!("could not reach auth service: {e}")))?;

    if first.status().as_u16() != 401 {
        return Ok(first);
    }

    // Access JWT missing/expired — refresh cookie may still be valid (7d).
    match refresh_session().await {
        Ok(_) => with_credentials(client().get(api_url(path)))
            .send()
            .await
            .map_err(|e| AuthError::Network(format!("could not reach auth service: {e}"))),
        Err(AuthError::Unauthorized(m)) => Err(AuthError::Unauthorized(m)),
        Err(AuthError::Message(m)) => Err(AuthError::Unauthorized(m)),
        Err(e) => Err(e),
    }
}

async fn authorized_post_json<B: Serialize + ?Sized>(
    path: &str,
    body: &B,
) -> Result<reqwest::Response, AuthError> {
    let first = with_credentials(client().post(api_url(path)).json(body))
        .send()
        .await
        .map_err(|e| AuthError::Network(format!("could not reach auth service: {e}")))?;

    if first.status().as_u16() != 401 {
        return Ok(first);
    }

    match refresh_session().await {
        Ok(_) => with_credentials(client().post(api_url(path)).json(body))
            .send()
            .await
            .map_err(|e| AuthError::Network(format!("could not reach auth service: {e}"))),
        Err(AuthError::Unauthorized(m)) => Err(AuthError::Unauthorized(m)),
        Err(AuthError::Message(m)) => Err(AuthError::Unauthorized(m)),
        Err(e) => Err(e),
    }
}

pub async fn signup(body: SignupBody) -> Result<AuthResponse, AuthError> {
    let res = with_credentials(client().post(api_url("/api/auth/signup")).json(&body))
        .send()
        .await
        .map_err(|e| AuthError::Network(format!("could not reach auth service: {e}")))?;
    parse_json_or_error(res).await
}

pub async fn login(body: LoginBody) -> Result<AuthResponse, AuthError> {
    let res = with_credentials(client().post(api_url("/api/auth/login")).json(&body))
        .send()
        .await
        .map_err(|e| AuthError::Network(format!("could not reach auth service: {e}")))?;
    parse_json_or_error(res).await
}

/// Current user + workspaces. Silently refreshes if the short-lived access cookie expired.
pub async fn me() -> Result<(UserPublic, Vec<WorkspacePublic>), AuthError> {
    #[derive(Deserialize)]
    struct MeResponse {
        user: UserPublic,
        #[serde(default)]
        workspaces: Vec<WorkspacePublic>,
    }
    let res = authorized_get("/api/auth/me").await?;
    let body: MeResponse = parse_json_or_error(res).await?;
    Ok((body.user, body.workspaces))
}

pub async fn list_workspaces() -> Result<Vec<WorkspacePublic>, AuthError> {
    #[derive(Deserialize)]
    struct ListResponse {
        workspaces: Vec<WorkspacePublic>,
    }
    let res = authorized_get("/api/workspaces").await?;
    let body: ListResponse = parse_json_or_error(res).await?;
    Ok(body.workspaces)
}

pub async fn create_workspace(body: CreateWorkspaceBody) -> Result<WorkspacePublic, AuthError> {
    #[derive(Deserialize)]
    struct CreateResponse {
        workspace: WorkspacePublic,
    }
    let res = authorized_post_json("/api/workspaces", &body).await?;
    let body: CreateResponse = parse_json_or_error(res).await?;
    Ok(body.workspace)
}

pub async fn logout() -> Result<(), AuthError> {
    let res = with_credentials(client().post(api_url("/api/auth/logout")))
        .send()
        .await
        .map_err(|e| AuthError::Network(format!("could not reach auth service: {e}")))?;
    if res.status().is_success() {
        Ok(())
    } else {
        // Non-success always Err — extracts the server message.
        parse_json_or_error::<ErrorBody>(res).await.map(|_| ())
    }
}

/// Flag for login page banner after successful signup.
pub fn mark_just_registered() {
    #[cfg(target_arch = "wasm32")]
    if let Some(storage) = web_sys::window()
        .and_then(|w| w.session_storage().ok().flatten())
    {
        let _ = storage.set_item("fleet_just_registered", "1");
    }
}

pub fn take_just_registered() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(storage) = web_sys::window()
            .and_then(|w| w.session_storage().ok().flatten())
        {
            let hit = storage
                .get_item("fleet_just_registered")
                .ok()
                .flatten()
                .as_deref()
                == Some("1");
            if hit {
                let _ = storage.remove_item("fleet_just_registered");
            }
            return hit;
        }
    }
    false
}
