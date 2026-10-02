//! Cookie-auth client for workspace worker hosts.

use serde::{Deserialize, Serialize};

use crate::pages::auth::api::{auth_base_url, refresh_session, with_credentials};

pub use fleet_runners_view::{
    CreateHostResponse, HostDesiredSoftware, HostPatch, HostPublic, PackageInventory,
    PackageVersions,
};

#[derive(Debug, Clone, Serialize, Default)]
pub struct CreateHostBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub send_policy: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct HostListResponse {
    #[serde(default)]
    hosts: Vec<HostPublic>,
    #[serde(default)]
    packages: Vec<HostDesiredSoftware>,
    #[serde(default)]
    plane_git_sha: Option<String>,
    #[serde(default)]
    package_versions: Option<PackageVersions>,
    #[serde(default)]
    worker_public_base: String,
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

fn apply_patch(host: HostPublic, patch: &HostPatch) -> HostPublic {
    fleet_runners_view::apply_patch(host, patch)
}

pub async fn list_workers_page(
    workspace_id: &str,
) -> Result<(Vec<HostPublic>, PackageInventory), String> {
    let res = authorized_get(&format!("/api/workspaces/{workspace_id}/worker-hosts")).await?;
    let body: HostListResponse = parse_json(res).await?;
    let plane = body.plane_git_sha.clone();
    let versions = body.package_versions.clone().unwrap_or_default();
    let hosts = body
        .hosts
        .into_iter()
        .map(|mut h| {
            h.plane_git_sha = plane.clone();
            h.current_versions = versions.clone();
            h
        })
        .collect();
    Ok((
        hosts,
        PackageInventory {
            packages: body.packages,
            plane_git_sha: body.plane_git_sha,
            package_versions: versions,
            worker_public_base: body.worker_public_base,
        },
    ))
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
    fleet_runners_view::install_commands(created, &auth_base_url())
}

/// PowerShell one-liner: download, pair, start (Windows).
pub fn windows_install_command(created: &CreateHostResponse) -> String {
    fleet_runners_view::windows_install_command(created, &auth_base_url())
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LocalModel {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LocalModelCatalog {
    #[serde(default)]
    pub models: Vec<LocalModel>,
    #[serde(default)]
    pub source: String,
}

/// `GET /api/workspaces/{id}/local-models` — live top-50 Ollama library.
pub async fn fetch_local_models(workspace_id: &str) -> Result<LocalModelCatalog, String> {
    let res = authorized_get(&format!("/api/workspaces/{workspace_id}/local-models")).await?;
    parse_json(res).await
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LocalModelTag {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub spec: String,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub ram_bytes: Option<u64>,
    #[serde(default)]
    pub latest: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LocalModelTags {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub tags: Vec<LocalModelTag>,
    #[serde(default)]
    pub source: String,
}

/// `GET /api/workspaces/{id}/local-models/{name}/tags`
pub async fn fetch_local_model_tags(
    workspace_id: &str,
    name: &str,
) -> Result<LocalModelTags, String> {
    let res = authorized_get(&format!(
        "/api/workspaces/{workspace_id}/local-models/{name}/tags"
    ))
    .await?;
    parse_json(res).await
}
