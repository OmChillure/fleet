//! Browser client for `fleet-auth` (cookies via credentials: include).

#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// Override at build time: `FLEET_AUTH_URL=https://api.example.com dx serve …`
/// Default matches local `fleet-auth` bind.
pub fn auth_base_url() -> String {
    option_env!("FLEET_AUTH_URL")
        .unwrap_or("http://127.0.0.1:3001")
        .to_string()
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

#[derive(Debug, Clone, Deserialize)]
pub struct UserPublic {
    pub id: String,
    pub email: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthResponse {
    pub user: UserPublic,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ErrorBody {
    pub error: String,
}

#[derive(Debug, Clone)]
pub enum AuthError {
    Message(String),
    Network(String),
}

impl AuthError {
    pub fn message(&self) -> String {
        match self {
            AuthError::Message(m) | AuthError::Network(m) => m.clone(),
        }
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

/// Attach credentials so httpOnly auth cookies are sent/stored cross-origin.
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

pub async fn signup(body: SignupBody) -> Result<AuthResponse, AuthError> {
    let url = format!("{}/api/auth/signup", auth_base_url());
    let res = with_credentials(client().post(url).json(&body))
        .send()
        .await
        .map_err(|e| AuthError::Network(format!("could not reach auth service: {e}")))?;
    parse_json_or_error(res).await
}

pub async fn login(body: LoginBody) -> Result<AuthResponse, AuthError> {
    let url = format!("{}/api/auth/login", auth_base_url());
    let res = with_credentials(client().post(url).json(&body))
        .send()
        .await
        .map_err(|e| AuthError::Network(format!("could not reach auth service: {e}")))?;
    parse_json_or_error(res).await
}

pub async fn me() -> Result<UserPublic, AuthError> {
    #[derive(Deserialize)]
    struct MeResponse {
        user: UserPublic,
    }
    let url = format!("{}/api/auth/me", auth_base_url());
    let res = with_credentials(client().get(url))
        .send()
        .await
        .map_err(|e| AuthError::Network(format!("could not reach auth service: {e}")))?;
    let body: MeResponse = parse_json_or_error(res).await?;
    Ok(body.user)
}

pub async fn logout() -> Result<(), AuthError> {
    let url = format!("{}/api/auth/logout", auth_base_url());
    let res = with_credentials(client().post(url))
        .send()
        .await
        .map_err(|e| AuthError::Network(format!("could not reach auth service: {e}")))?;
    if res.status().is_success() {
        Ok(())
    } else {
        let _ = parse_json_or_error::<ErrorBody>(res).await?;
        Ok(())
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
