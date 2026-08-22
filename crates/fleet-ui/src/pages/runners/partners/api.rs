//! Partners disclaimer ack client (fetch / post only).
//!
//! B2 (`page.rs`) should call:
//!   `fetch_disclaimer_ack(workspace_id) -> Result<DisclaimerAck, String>`
//!   `post_disclaimer_ack(workspace_id) -> Result<DisclaimerAck, String>`
//!
//! `accepted` is a recorded timestamp only. It does **not** enable
//! lending, borrowing, enqueue, or assignable partner runners.

use serde::{Deserialize, Serialize};

use crate::pages::auth::api::{auth_base_url, refresh_session};

/// Workspace-level disclaimer ack. Default is not accepted.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct DisclaimerAck {
    #[serde(default)]
    pub accepted: bool,
    #[serde(default)]
    pub accepted_at: Option<String>,
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

fn api_url(path: &str) -> String {
    format!(
        "{}/{}",
        auth_base_url().trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[allow(unused_mut)]
fn with_credentials(mut req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    #[cfg(target_arch = "wasm32")]
    {
        req = req.fetch_credentials_include();
    }
    req
}

async fn authorized_get(path: &str) -> Result<reqwest::Response, String> {
    let first = with_credentials(client().get(api_url(path)))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if first.status().as_u16() != 401 {
        return Ok(first);
    }
    refresh_session().await.map_err(|e| e.message())?;
    with_credentials(client().get(api_url(path)))
        .send()
        .await
        .map_err(|e| e.to_string())
}

async fn authorized_post_json(
    path: &str,
    body: &impl Serialize,
) -> Result<reqwest::Response, String> {
    let first = with_credentials(client().post(api_url(path)).json(body))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if first.status().as_u16() != 401 {
        return Ok(first);
    }
    refresh_session().await.map_err(|e| e.message())?;
    with_credentials(client().post(api_url(path)).json(body))
        .send()
        .await
        .map_err(|e| e.to_string())
}

async fn parse_json<T: for<'de> Deserialize<'de>>(res: reqwest::Response) -> Result<T, String> {
    let status = res.status();
    let text = res.text().await.map_err(|e| e.to_string())?;
    if status.is_success() {
        serde_json::from_str(&text).map_err(|e| format!("invalid JSON: {e}"))
    } else {
        #[derive(Deserialize)]
        struct ErrBody {
            error: String,
        }
        Err(serde_json::from_str::<ErrBody>(&text)
            .map(|b| b.error)
            .unwrap_or_else(|_| format!("request failed ({status})")))
    }
}

fn disclaimer_path(workspace_id: &str) -> String {
    format!("/api/workspaces/{workspace_id}/partners/disclaimer")
}

pub async fn fetch_disclaimer_ack(workspace_id: &str) -> Result<DisclaimerAck, String> {
    let res = authorized_get(&disclaimer_path(workspace_id)).await?;
    parse_json(res).await
}

pub async fn post_disclaimer_ack(workspace_id: &str) -> Result<DisclaimerAck, String> {
    let res = authorized_post_json(&disclaimer_path(workspace_id), &serde_json::json!({})).await?;
    parse_json(res).await
}
