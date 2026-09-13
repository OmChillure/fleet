//! Workspace fleet settings (max agents + local fleet LLM).

use serde::{Deserialize, Serialize};

use crate::pages::auth::api::{auth_base_url, refresh_session, with_credentials};

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FleetSettings {
    pub max_agents: i32,
    #[serde(default = "default_cap")]
    pub max_agents_cap: i32,
    #[serde(default = "default_process")]
    pub min_isolation: String,
    #[serde(default = "default_on_pc")]
    pub default_run_mode: String,
    #[serde(default = "default_chat_max_turns")]
    pub chat_max_turns: i32,
    #[serde(default = "default_chat_max_turns_min")]
    pub chat_max_turns_min: i32,
    #[serde(default = "default_chat_max_turns_max")]
    pub chat_max_turns_max: i32,
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

fn default_process() -> String {
    "process".into()
}

fn default_on_pc() -> String {
    "on_pc".into()
}

fn default_chat_max_turns() -> i32 {
    32
}

fn default_chat_max_turns_min() -> i32 {
    1
}

fn default_chat_max_turns_max() -> i32 {
    128
}

#[derive(Debug, Clone, Serialize)]
pub struct FleetSettingsPatch {
    pub max_agents: i32,
    pub fleet_llm: String,
    pub fleet_llm_model: String,
    pub min_isolation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_run_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_max_turns: Option<i32>,
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

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LlmKeyPublic {
    pub id: String,
    pub provider: String,
    pub created_at: String,
    pub revoked: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct LlmKeyList {
    #[serde(default)]
    keys: Vec<LlmKeyPublic>,
}

pub async fn list_llm_keys(workspace_id: &str) -> Result<Vec<LlmKeyPublic>, String> {
    let res = authorized_get(&format!("/api/workspaces/{workspace_id}/llm-keys")).await?;
    let body: LlmKeyList = parse_json(res).await?;
    Ok(body.keys)
}

pub async fn create_llm_key(
    workspace_id: &str,
    provider: &str,
    secret: &str,
) -> Result<LlmKeyPublic, String> {
    let res = authorized_post_json(
        &format!("/api/workspaces/{workspace_id}/llm-keys"),
        &serde_json::json!({ "provider": provider, "secret": secret }),
    )
    .await?;
    #[derive(Deserialize)]
    struct Wrap {
        key: LlmKeyPublic,
    }
    let body: Wrap = parse_json(res).await?;
    Ok(body.key)
}

pub async fn revoke_llm_key(workspace_id: &str, key_id: &str) -> Result<(), String> {
    let res = authorized_post_json(
        &format!("/api/workspaces/{workspace_id}/llm-keys/{key_id}/revoke"),
        &serde_json::json!({}),
    )
    .await?;
    let status = res.status();
    if status.is_success() {
        Ok(())
    } else {
        let text = res.text().await.unwrap_or_default();
        Err(if text.is_empty() {
            format!("request failed ({status})")
        } else {
            text
        })
    }
}

pub async fn fetch_settings(workspace_id: &str) -> Result<FleetSettings, String> {
    let res = authorized_get(&format!("/api/workspaces/{workspace_id}/fleet/settings")).await?;
    parse_json(res).await
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

async fn authorized_delete(path: &str) -> Result<reqwest::Response, String> {
    let first = with_credentials(client().delete(api_url(path)))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if first.status().as_u16() != 401 {
        return Ok(first);
    }
    refresh_session().await.map_err(|e| e.message())?;
    with_credentials(client().delete(api_url(path)))
        .send()
        .await
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MemberRow {
    pub user_id: String,
    pub email: String,
    #[serde(default)]
    pub display_name: Option<String>,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MembersResponse {
    #[serde(default)]
    pub members: Vec<MemberRow>,
    pub you: MemberYou,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MemberYou {
    pub user_id: String,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct InviteRow {
    pub id: String,
    pub email: String,
    pub role: String,
    #[serde(default)]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InvitesResponse {
    #[serde(default)]
    pub invites: Vec<InviteRow>,
}

pub async fn list_members(workspace_id: &str) -> Result<MembersResponse, String> {
    let res = authorized_get(&format!("/api/workspaces/{workspace_id}/members")).await?;
    parse_json(res).await
}

pub async fn invite_member(
    workspace_id: &str,
    email: &str,
    role: &str,
) -> Result<serde_json::Value, String> {
    let res = authorized_post_json(
        &format!("/api/workspaces/{workspace_id}/invites"),
        &serde_json::json!({ "email": email, "role": role }),
    )
    .await?;
    parse_json(res).await
}

pub async fn list_invites(workspace_id: &str) -> Result<Vec<InviteRow>, String> {
    let res = authorized_get(&format!("/api/workspaces/{workspace_id}/invites")).await?;
    let body: InvitesResponse = parse_json(res).await?;
    Ok(body.invites)
}

pub async fn revoke_invite(workspace_id: &str, invite_id: &str) -> Result<(), String> {
    let res = authorized_delete(&format!(
        "/api/workspaces/{workspace_id}/invites/{invite_id}"
    ))
    .await?;
    let _: serde_json::Value = parse_json(res).await?;
    Ok(())
}

pub async fn set_member_role(
    workspace_id: &str,
    user_id: &str,
    role: &str,
) -> Result<MemberRow, String> {
    let res = authorized_patch_json(
        &format!("/api/workspaces/{workspace_id}/members/{user_id}"),
        &serde_json::json!({ "role": role }),
    )
    .await?;
    parse_json(res).await
}

pub async fn remove_member(workspace_id: &str, user_id: &str) -> Result<(), String> {
    let res =
        authorized_delete(&format!("/api/workspaces/{workspace_id}/members/{user_id}")).await?;
    let _: serde_json::Value = parse_json(res).await?;
    Ok(())
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
