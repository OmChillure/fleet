//! Control-plane HTTP: pair, heartbeat, lease, complete.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use uuid::Uuid;

use crate::config::LocalConfig;
use crate::platform::{advertised_container, advertised_host, detect_platform, hostname};

#[derive(Debug, Error)]
pub enum ApiError {
    #[error("machine token rejected ({0}); re-pair with `fleet-host pair`")]
    Auth(StatusCode),
    #[error("HTTP {status}: {body}")]
    Http { status: StatusCode, body: String },
}

#[derive(Clone)]
pub struct HostClient {
    http: Client,
    base: String,
    token: Option<String>,
}

impl HostClient {
    pub fn for_pair(base: &str, cfg: &LocalConfig) -> Result<Self> {
        Self::build(base, None, cfg)
    }

    pub fn with_machine_token(
        base: &str,
        token: impl Into<String>,
        cfg: &LocalConfig,
    ) -> Result<Self> {
        Self::build(base, Some(token.into()), cfg)
    }

    fn build(base: &str, token: Option<String>, cfg: &LocalConfig) -> Result<Self> {
        let base = normalize_base(base);
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            bail!("control plane URL must be http(s): {base}");
        }
        let mut b = Client::builder()
            .user_agent(format!("fleet-host/{}", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10));
        if let Some(proxy) = cfg
            .http_proxy
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            b = b.proxy(reqwest::Proxy::all(proxy).with_context(|| format!("http_proxy {proxy}"))?);
        }
        Ok(Self {
            http: b.build().context("reqwest client")?,
            base,
            token,
        })
    }

    fn url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.base.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    fn bearer(&self) -> Result<String> {
        self.token
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("missing machine token"))
    }

    pub async fn pair(&self, cfg: &LocalConfig, pair_token: &str) -> Result<PairResponse> {
        let body = PairRequest {
            token: pair_token.to_string(),
            hostname: hostname(),
            platform: detect_platform(),
            advertised_host: advertised_host(cfg),
            advertised_container: advertised_container(cfg),
        };
        let resp = self
            .http
            .post(self.url("/api/worker-hosts/pair"))
            .json(&body)
            .send()
            .await
            .context("POST /api/worker-hosts/pair")?;
        read_json(resp).await
    }

    pub async fn heartbeat(
        &self,
        cfg: &LocalConfig,
        current_jobs: u32,
        error: Option<&str>,
    ) -> Result<HeartbeatResponse> {
        let token = self.bearer()?;
        let body = HeartbeatRequest {
            advertised_host: advertised_host(cfg),
            advertised_container: advertised_container(cfg),
            platform: Some(detect_platform()),
            hostname: Some(hostname()),
            current_jobs,
            max_concurrent: cfg.max_concurrent,
            error: error.map(|s| s.to_string()),
            software: Some(crate::software::heartbeat_software(cfg)),
        };
        let resp = self
            .http
            .post(self.url("/api/worker-hosts/me/heartbeat"))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .context("POST /api/worker-hosts/me/heartbeat")?;
        reject_auth(resp.status())?;
        read_json(resp).await
    }

    pub async fn lease(&self) -> Result<Option<LeaseClaim>> {
        let token = self.bearer()?;
        let resp = self
            .http
            .post(self.url("/api/worker-hosts/me/lease"))
            .bearer_auth(&token)
            .json(&serde_json::json!({}))
            .send()
            .await
            .context("POST /api/worker-hosts/me/lease")?;
        reject_auth(resp.status())?;
        let parsed: LeaseResponse = read_json(resp).await?;
        Ok(parsed.claim)
    }

    pub async fn complete(
        &self,
        claim_id: Uuid,
        lease_token: &str,
        ok: bool,
        error: Option<&str>,
    ) -> Result<()> {
        let token = self.bearer()?;
        let body = CompleteRequest {
            lease_token: lease_token.to_string(),
            ok,
            error: error.map(|s| s.to_string()),
        };
        let path = format!("/api/worker-hosts/me/lease/{claim_id}/complete");
        let resp = self
            .http
            .post(self.url(&path))
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        reject_auth(resp.status())?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("HTTP {status}: {}", text.trim());
        }
        Ok(())
    }
}

fn normalize_base(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

fn reject_auth(status: StatusCode) -> Result<()> {
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(ApiError::Auth(status).into());
    }
    Ok(())
}

async fn read_json<T: DeserializeOwned>(resp: Response) -> Result<T> {
    let status = resp.status();
    let bytes = resp.bytes().await.context("read response body")?;
    if !status.is_success() {
        let body = String::from_utf8_lossy(&bytes);
        bail!("HTTP {status}: {}", body.trim());
    }
    serde_json::from_slice(&bytes).with_context(|| {
        format!(
            "decode JSON ({status}): {}",
            String::from_utf8_lossy(&bytes)
        )
    })
}

#[derive(Debug, Serialize)]
struct PairRequest {
    token: String,
    hostname: String,
    platform: String,
    advertised_host: bool,
    advertised_container: bool,
}

#[derive(Debug, Deserialize)]
pub struct PairResponse {
    pub host_id: Uuid,
    pub machine_token: String,
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
    #[serde(default)]
    pub control_plane_url: Option<String>,
    #[serde(default)]
    pub cp_settings: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct HeartbeatRequest {
    advertised_host: bool,
    advertised_container: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hostname: Option<String>,
    current_jobs: u32,
    max_concurrent: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    software: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HeartbeatResponse {
    #[serde(default)]
    pub host: HostPublic,
    #[serde(default)]
    pub effective: EffectiveModes,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct HostPublic {
    #[serde(default)]
    pub id: Option<Uuid>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub paused: Option<bool>,
    #[serde(default)]
    pub drain: Option<bool>,
}

impl HostPublic {
    pub fn accepts_leases(&self) -> bool {
        if self.enabled == Some(false) {
            return false;
        }
        if self.paused == Some(true) || self.drain == Some(true) {
            return false;
        }
        if let Some(s) = self.status.as_deref() {
            if s.eq_ignore_ascii_case("disabled") || s.eq_ignore_ascii_case("error") {
                return false;
            }
        }
        true
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct EffectiveModes {
    #[serde(default)]
    pub host: bool,
    #[serde(default)]
    pub container: bool,
    #[serde(default)]
    pub max_concurrent: u32,
}

impl Default for EffectiveModes {
    fn default() -> Self {
        Self {
            host: true,
            container: false,
            max_concurrent: 8,
        }
    }
}

impl EffectiveModes {
    pub fn from_local(cfg: &LocalConfig) -> Self {
        Self {
            host: advertised_host(cfg),
            container: advertised_container(cfg),
            max_concurrent: cfg.local_max_jobs(),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
struct LeaseResponse {
    #[serde(default)]
    claim: Option<LeaseClaim>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeaseClaim {
    pub id: Uuid,
    pub lease_token: String,
    pub payload: SpawnPayload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobRuntime {
    Host,
    Container,
}

impl JobRuntime {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Container => "container",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpawnPayload {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub session_id: Uuid,
    #[serde(default)]
    pub project_id: Uuid,
    #[serde(default)]
    pub run_id: Uuid,
    #[serde(default)]
    pub board_task_id: Uuid,
    #[serde(default)]
    pub plan_task_id: String,
    #[serde(default)]
    pub worker_profile: String,
    #[serde(default)]
    pub worker_token: String,
    #[serde(default)]
    pub orchestrator_url: String,
    pub runtime: JobRuntime,
    #[serde(default)]
    pub task_title: String,
    #[serde(default)]
    pub task_spec: String,
    #[serde(default = "default_timeout_s")]
    pub timeout_s: u64,
    #[serde(default)]
    pub repo_url: Option<String>,
    #[serde(default)]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub privileged: bool,
}

fn default_timeout_s() -> u64 {
    3600
}

#[derive(Debug, Serialize)]
struct CompleteRequest {
    lease_token: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}
