//! Browser client for agent personas.

use serde::Deserialize;

use crate::pages::auth::api::{auth_base_url, refresh_session};

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PersonaDto {
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub division: String,
    #[serde(default)]
    pub emoji: String,
    #[serde(default)]
    pub vibe: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PersonasResponse {
    #[serde(default)]
    source: String,
    #[serde(default)]
    agent_backend: Option<String>,
    #[serde(default)]
    personas: Vec<PersonaDto>,
}

pub struct PersonasPayload {
    pub source: String,
    pub agent_backend: Option<String>,
    pub personas: Vec<PersonaDto>,
}

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn authorized_get(path: &str) -> Result<reqwest::Response, String> {
    let url = format!(
        "{}/{}",
        auth_base_url().trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    let mut req = client().get(url);
    #[cfg(target_arch = "wasm32")]
    {
        req = req.fetch_credentials_include();
    }
    let first = req.send().await.map_err(|e| e.to_string())?;
    if first.status().as_u16() != 401 {
        return Ok(first);
    }
    refresh_session().await.map_err(|e| e.message())?;
    let url = format!(
        "{}/{}",
        auth_base_url().trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    let mut req = client().get(url);
    #[cfg(target_arch = "wasm32")]
    {
        req = req.fetch_credentials_include();
    }
    req.send().await.map_err(|e| e.to_string())
}

pub async fn fetch_personas(workspace_id: &str) -> Result<PersonasPayload, String> {
    let res = authorized_get(&format!("/api/workspaces/{workspace_id}/fleet/personas")).await?;
    let status = res.status();
    let text = res.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("personas failed ({status})"));
    }
    let body: PersonasResponse = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(PersonasPayload {
        source: body.source,
        agent_backend: body.agent_backend,
        personas: body.personas,
    })
}
