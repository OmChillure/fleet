//! Partners disclaimer + lend/borrow client.

use serde::{Deserialize, Serialize};

use crate::pages::auth::api::{auth_base_url, refresh_session, with_credentials};

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

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PartnerOffer {
    pub id: String,
    pub host_id: String,
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub tier: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PartnerOfferList {
    #[serde(default)]
    pub offers: Vec<PartnerOffer>,
    #[serde(default)]
    pub killed: bool,
}

pub async fn list_partner_offers(workspace_id: &str) -> Result<PartnerOfferList, String> {
    let res = authorized_get(&format!(
        "/api/workspaces/{workspace_id}/runners/partners/offers"
    ))
    .await?;
    parse_json(res).await
}

pub async fn offer_partner_host(workspace_id: &str, host_id: &str) -> Result<(), String> {
    let res = authorized_post_json(
        &format!("/api/workspaces/{workspace_id}/runners/partners/offer"),
        &serde_json::json!({ "host_id": host_id }),
    )
    .await?;
    let _: serde_json::Value = parse_json(res).await?;
    Ok(())
}

pub async fn borrow_partner_offer(
    workspace_id: &str,
    offer_id: &str,
    allow_open: bool,
) -> Result<(), String> {
    let res = authorized_post_json(
        &format!("/api/workspaces/{workspace_id}/runners/partners/borrow"),
        &serde_json::json!({ "offer_id": offer_id, "allow_open": allow_open }),
    )
    .await?;
    let _: serde_json::Value = parse_json(res).await?;
    Ok(())
}
