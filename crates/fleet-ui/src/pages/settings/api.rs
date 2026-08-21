//! Workspace fleet settings (max agents + local fleet LLM).

use serde::{Deserialize, Serialize};

use crate::pages::auth::api::{auth_base_url, refresh_session};

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FleetSettings {
    pub max_agents: i32,
    #[serde(default = "default_cap")]
    pub max_agents_cap: i32,
    #[serde(default)]
    pub sandbox_backend: String,
    #[serde(default = "default_off")]
    pub fleet_llm: String,
    #[serde(default)]
    pub fleet_llm_model: String,
    #[serde(default)]
    pub grok_env: bool,
    #[serde(default)]
    pub effective_backend: Option<String>,
    #[serde(default)]
    pub effective_model: Option<String>,
}

fn default_cap() -> i32 {
    256
}

fn default_off() -> String {
    "off".into()
}

#[derive(Debug, Clone, Serialize)]
pub struct FleetSettingsPatch {
    pub max_agents: i32,
    pub fleet_llm: String,
    pub fleet_llm_model: String,
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

async fn authorized_patch_json(
    path: &str,
    body: &impl Serialize,
) -> Result<reqwest::Response, String> {
    let first = with_credentials(client().patch(api_url(path)).json(body))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if first.status().as_u16() != 401 {
        return Ok(first);
    }
    refresh_session().await.map_err(|e| e.message())?;
    with_credentials(client().patch(api_url(path)).json(body))
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

pub async fn fetch_settings(workspace_id: &str) -> Result<FleetSettings, String> {
    let res = authorized_get(&format!("/api/workspaces/{workspace_id}/fleet/settings")).await?;
    parse_json(res).await
}

pub async fn save_settings(
    workspace_id: &str,
    patch: &FleetSettingsPatch,
) -> Result<FleetSettings, String> {
    let res = authorized_patch_json(
        &format!("/api/workspaces/{workspace_id}/fleet/settings"),
        patch,
    )
    .await?;
    parse_json(res).await
}
