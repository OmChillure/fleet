//! Browser client for workspace personas and roster specialists.

use serde::Deserialize;

use crate::pages::auth::api::{auth_base_url, refresh_session, with_credentials};

/// Agency-agents roster specialist (`GET …/fleet/personas`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RosterSpecialist {
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
struct RosterSpecialistsResponse {
    #[serde(default)]
    source: String,
    #[serde(default)]
    agent_backend: Option<String>,
    /// Wire key on `/fleet/personas` — roster specialists only.
    #[serde(default)]
    personas: Vec<RosterSpecialist>,
}

pub struct RosterSpecialistsPayload {
    pub source: String,
    pub agent_backend: Option<String>,
    pub specialists: Vec<RosterSpecialist>,
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
    let first = with_credentials(client().get(url))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if first.status().as_u16() != 401 {
        return Ok(first);
    }
    refresh_session().await.map_err(|e| e.message())?;
    let url = format!(
        "{}/{}",
        auth_base_url().trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    with_credentials(client().get(url))
        .send()
        .await
        .map_err(|e| e.to_string())
}

pub async fn fetch_roster_specialists(
    workspace_id: &str,
) -> Result<RosterSpecialistsPayload, String> {
    let res = authorized_get(&format!("/api/workspaces/{workspace_id}/fleet/personas")).await?;
    let status = res.status();
    let text = res.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("roster specialists failed ({status})"));
    }
    let body: RosterSpecialistsResponse = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(RosterSpecialistsPayload {
        source: body.source,
        agent_backend: body.agent_backend,
        specialists: body.personas,
    })
}

/// Workspace persona (`GET …/personas`). Not a roster specialist.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WorkspacePersona {
    pub id: String,
    pub slug: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub worker_profile: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub is_starter: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct WorkspacePersonaList {
    #[serde(default)]
    personas: Vec<WorkspacePersona>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SeedResult {
    #[serde(default)]
    pub inserted: usize,
    #[serde(default)]
    pub skipped: usize,
}

async fn authorized_send(
    method: &str,
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<reqwest::Response, String> {
    let url = format!(
        "{}/{}",
        auth_base_url().trim_end_matches('/'),
        path.trim_start_matches('/')
    );
    async fn send_once(
        method: &str,
        url: &str,
        body: &Option<serde_json::Value>,
    ) -> Result<reqwest::Response, String> {
        let mut req = match method {
            "POST" => client().post(url),
            "DELETE" => client().delete(url),
            _ => client().get(url),
        };
        #[cfg(target_arch = "wasm32")]
        {
            req = req.fetch_credentials_include();
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        req.send().await.map_err(|e| e.to_string())
    }
    let first = send_once(method, &url, &body).await?;
    if first.status().as_u16() != 401 {
        return Ok(first);
    }
    refresh_session().await.map_err(|e| e.message())?;
    send_once(method, &url, &body).await
}

fn error_from(status: reqwest::StatusCode, text: &str) -> String {
    #[derive(Deserialize)]
    struct ErrBody {
        error: String,
    }
    serde_json::from_str::<ErrBody>(text)
        .map(|b| b.error)
        .unwrap_or_else(|_| format!("request failed ({status})"))
}

pub async fn list_workspace_personas(workspace_id: &str) -> Result<Vec<WorkspacePersona>, String> {
    let res = authorized_get(&format!("/api/workspaces/{workspace_id}/personas")).await?;
    let status = res.status();
    let text = res.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(error_from(status, &text));
    }
    let body: WorkspacePersonaList = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    Ok(body.personas)
}

pub async fn seed_starters(workspace_id: &str) -> Result<SeedResult, String> {
    let res = authorized_send(
        "POST",
        &format!("/api/workspaces/{workspace_id}/personas/seed-starters"),
        Some(serde_json::json!({})),
    )
    .await?;
    let status = res.status();
    let text = res.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(error_from(status, &text));
    }
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

pub async fn create_workspace_persona(
    workspace_id: &str,
    name: &str,
    description: &str,
    role_prompt: &str,
    worker_profile: &str,
) -> Result<WorkspacePersona, String> {
    let res = authorized_send(
        "POST",
        &format!("/api/workspaces/{workspace_id}/personas"),
        Some(serde_json::json!({
            "name": name,
            "description": description,
            "role_prompt": role_prompt,
            "worker_profile": worker_profile,
        })),
    )
    .await?;
    let status = res.status();
    let text = res.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(error_from(status, &text));
    }
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

pub async fn archive_workspace_persona(workspace_id: &str, persona_id: &str) -> Result<(), String> {
    let res = authorized_send(
        "DELETE",
        &format!("/api/workspaces/{workspace_id}/personas/{persona_id}"),
        None,
    )
    .await?;
    let status = res.status();
    if status.as_u16() == 204 || status.is_success() {
        return Ok(());
    }
    let text = res.text().await.map_err(|e| e.to_string())?;
    Err(error_from(status, &text))
}
