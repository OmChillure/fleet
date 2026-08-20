//! Cookie-auth client for workspace worker hosts.

use serde::{Deserialize, Serialize};

use crate::pages::auth::api::{auth_base_url, refresh_session};

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HostPublic {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub status: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub drain: bool,
    #[serde(default = "default_true")]
    pub allow_host: bool,
    #[serde(default = "default_true")]
    pub allow_container: bool,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: i32,
    #[serde(default = "default_timeout")]
    pub max_job_timeout_s: i32,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub advertised_host: bool,
    #[serde(default)]
    pub advertised_container: bool,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub last_seen_at: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub paired_at: Option<String>,
    #[serde(default)]
    pub software: Option<HostSoftware>,
    #[serde(default = "default_true")]
    pub auto_update: bool,
    #[serde(default)]
    pub current_jobs: i32,
    #[serde(default)]
    pub advertised_max_concurrent: Option<i32>,
    #[serde(default)]
    pub hardware: Option<HostHardware>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HostHardware {
    #[serde(default)]
    pub cpu_cores: Option<u32>,
    #[serde(default)]
    pub mem_total_bytes: Option<u64>,
    #[serde(default)]
    pub mem_used_pct: Option<u8>,
    #[serde(default)]
    pub os: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HostSoftware {
    #[serde(default)]
    pub git_sha: Option<String>,
    #[serde(default)]
    pub updating: bool,
}

fn default_true() -> bool {
    true
}

fn default_max_concurrent() -> i32 {
    4
}

fn default_timeout() -> i32 {
    3600
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CreateHostResponse {
    pub host: HostPublic,
    pub pair_token: String,
    #[serde(default)]
    pub pair_expires_at: String,
    #[serde(default)]
    pub install_hint: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct CreateHostBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct HostPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_host: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_container: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
struct HostListResponse {
    #[serde(default)]
    hosts: Vec<HostPublic>,
}

#[derive(Debug, Clone, Deserialize)]
struct HostEnvelope {
    host: HostPublic,
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

async fn parse_json<T: for<'de> Deserialize<'de>>(res: reqwest::Response) -> Result<T, String> {
    let status = res.status();
    let text = res.text().await.map_err(|e| e.to_string())?;
    if status.is_success() {
        serde_json::from_str(&text).map_err(|e| format!("invalid JSON: {e}"))
    } else {
        Err(error_message(status, &text))
    }
}

fn error_message(status: reqwest::StatusCode, text: &str) -> String {
    #[derive(Deserialize)]
    struct ErrBody {
        error: String,
    }
    serde_json::from_str::<ErrBody>(text)
        .map(|b| b.error)
        .unwrap_or_else(|_| {
            if text.is_empty() {
                format!("request failed ({status})")
            } else {
                text.to_string()
            }
        })
}

fn apply_patch(mut host: HostPublic, patch: &HostPatch) -> HostPublic {
    if let Some(v) = patch.name.clone() {
        host.name = v;
    }
    if let Some(v) = patch.paused {
        host.paused = v;
    }
    if let Some(v) = patch.allow_host {
        host.allow_host = v;
    }
    if let Some(v) = patch.allow_container {
        host.allow_container = v;
    }
    if let Some(v) = patch.max_concurrent {
        host.max_concurrent = v;
    }
    host
}

/// `GET /api/workspaces/{id}/worker-hosts`
pub async fn list_hosts(workspace_id: &str) -> Result<Vec<HostPublic>, String> {
    let res = authorized_get(&format!("/api/workspaces/{workspace_id}/worker-hosts")).await?;
    let body: HostListResponse = parse_json(res).await?;
    Ok(body.hosts)
}

/// `POST /api/workspaces/{id}/worker-hosts` — pair token is in the response once.
pub async fn create_host(
    workspace_id: &str,
    body: &CreateHostBody,
) -> Result<CreateHostResponse, String> {
    let res = authorized_post_json(
        &format!("/api/workspaces/{workspace_id}/worker-hosts"),
        body,
    )
    .await?;
    parse_json(res).await
}

/// `PATCH /api/workspaces/{id}/worker-hosts/{hid}`
pub async fn patch_host(
    workspace_id: &str,
    host_id: &str,
    current: &HostPublic,
    patch: &HostPatch,
) -> Result<HostPublic, String> {
    let res = authorized_patch_json(
        &format!("/api/workspaces/{workspace_id}/worker-hosts/{host_id}"),
        patch,
    )
    .await?;
    let status = res.status();
    let text = res.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(error_message(status, &text));
    }
    if let Ok(host) = serde_json::from_str::<HostPublic>(&text) {
        return Ok(host);
    }
    if let Ok(wrap) = serde_json::from_str::<HostEnvelope>(&text) {
        return Ok(wrap.host);
    }
    Ok(apply_patch(current.clone(), patch))
}

/// `DELETE /api/workspaces/{id}/worker-hosts/{hid}`
pub async fn revoke_host(workspace_id: &str, host_id: &str) -> Result<(), String> {
    let res = authorized_delete(&format!(
        "/api/workspaces/{workspace_id}/worker-hosts/{host_id}"
    ))
    .await?;
    let status = res.status();
    if status.is_success() {
        return Ok(());
    }
    let text = res.text().await.map_err(|e| e.to_string())?;
    Err(error_message(status, &text))
}

/// Install + pair commands from `install_hint`, or a same-origin fallback.
pub fn install_commands(created: &CreateHostResponse) -> String {
    let hint = created.install_hint.trim();
    if !hint.is_empty() {
        return hint.to_string();
    }
    let api = auth_base_url();
    format!(
        "curl -fsSL {api}/install-worker.sh | FLEET_URL={api} FLEET_PAIR_TOKEN={} sh",
        created.pair_token
    )
}

/// Control-plane URL the install command will call (from hint, else page origin).
pub fn install_api_url(created: &CreateHostResponse) -> String {
    let hint = install_commands(created);
    if let Some(rest) = hint.split("FLEET_URL=").nth(1) {
        let url = rest
            .split(|c: char| c.is_whitespace() || c == '|')
            .next()
            .unwrap_or("")
            .trim_matches(|c| c == '\'' || c == '"');
        if url.starts_with("http") {
            return url.to_string();
        }
    }
    auth_base_url()
}

/// PowerShell one-liner: download, pair, start (Windows).
pub fn windows_install_command(created: &CreateHostResponse) -> String {
    let api = install_api_url(created);
    format!(
        "$env:FLEET_URL='{api}'; $env:FLEET_PAIR_TOKEN='{}'; irm \"$env:FLEET_URL/install-worker.ps1\" | iex",
        created.pair_token
    )
}
