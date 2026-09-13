//! Browser client for `/api/admin/*`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::pages::auth::api::{auth_base_url, refresh_session, with_credentials};

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

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct Overview {
    #[serde(default)]
    pub users: OverviewUsers,
    #[serde(default)]
    pub workspaces: i64,
    #[serde(default)]
    pub runners: std::collections::BTreeMap<String, i64>,
    #[serde(default)]
    pub sessions: std::collections::BTreeMap<String, i64>,
    #[serde(default)]
    pub work_claims: std::collections::BTreeMap<String, i64>,
    #[serde(default)]
    pub cases: std::collections::BTreeMap<String, i64>,
    #[serde(default)]
    pub goals: std::collections::BTreeMap<String, i64>,
    #[serde(default)]
    pub approvals: std::collections::BTreeMap<String, i64>,
    #[serde(default)]
    pub inbox: std::collections::BTreeMap<String, i64>,
    #[serde(default)]
    pub inbox_monitors: i64,
    #[serde(default)]
    pub unpublished_events: i64,
    #[serde(default)]
    pub pending_invites: i64,
    #[serde(default)]
    pub connected_integrations: i64,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
pub struct OverviewUsers {
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub by_status: std::collections::BTreeMap<String, i64>,
    #[serde(default)]
    pub instance_admins: i64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AdminUser {
    pub id: String,
    pub email: String,
    pub display_name: Option<String>,
    pub status: String,
    pub instance_admin: bool,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub workspace_count: i64,
    pub last_seen_at: Option<String>,
    #[serde(default)]
    pub active_sessions: i64,
    #[serde(default)]
    pub runners_created: i64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UsersResponse {
    pub you: YouRef,
    pub users: Vec<AdminUser>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct YouRef {
    pub id: String,
    pub email: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AdminWorkspace {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub created_at: String,
    #[serde(default)]
    pub min_isolation: String,
    #[serde(default)]
    pub member_count: i64,
    #[serde(default)]
    pub runner_count: i64,
    #[serde(default)]
    pub online_runners: i64,
    #[serde(default)]
    pub connected_integrations: i64,
    #[serde(default)]
    pub inbox_monitor: bool,
    #[serde(default)]
    pub partners_disclaimer: bool,
    pub owners: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PatchUserBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_admin: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

pub async fn fetch_overview() -> Result<Overview, String> {
    parse_json(authorized_get("/api/admin/overview").await?).await
}

pub async fn fetch_users() -> Result<UsersResponse, String> {
    parse_json(authorized_get("/api/admin/users").await?).await
}

pub async fn patch_user(user_id: &str, body: &PatchUserBody) -> Result<Value, String> {
    parse_json(authorized_patch_json(&format!("/api/admin/users/{user_id}"), body).await?).await
}

pub async fn fetch_workspaces() -> Result<Vec<AdminWorkspace>, String> {
    #[derive(Deserialize)]
    struct Wrap {
        workspaces: Vec<AdminWorkspace>,
    }
    let w: Wrap = parse_json(authorized_get("/api/admin/workspaces").await?).await?;
    Ok(w.workspaces)
}

pub async fn fetch_runners() -> Result<Vec<Value>, String> {
    #[derive(Deserialize)]
    struct Wrap {
        runners: Vec<Value>,
    }
    let w: Wrap = parse_json(authorized_get("/api/admin/runners").await?).await?;
    Ok(w.runners)
}

pub async fn fetch_instance() -> Result<Value, String> {
    parse_json(authorized_get("/api/admin/instance").await?).await
}

pub async fn fetch_work() -> Result<Value, String> {
    parse_json(authorized_get("/api/admin/work").await?).await
}

pub async fn fetch_sessions() -> Result<Vec<Value>, String> {
    #[derive(Deserialize)]
    struct Wrap {
        sessions: Vec<Value>,
    }
    let w: Wrap = parse_json(authorized_get("/api/admin/sessions").await?).await?;
    Ok(w.sessions)
}
