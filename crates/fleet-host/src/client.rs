//! Control-plane HTTP: pair, heartbeat, lease, complete.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
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
    settings_ack: Arc<AtomicI32>,
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
            settings_ack: Arc::new(AtomicI32::new(0)),
        })
    }

    fn take_settings_ack(&self) -> Option<i32> {
        let n = self.settings_ack.load(Ordering::SeqCst);
        if n > 0 {
            Some(n)
        } else {
            None
        }
    }

    fn note_settings_gen(&self, n: i32) {
        if n > 0 {
            self.settings_ack.store(n, Ordering::SeqCst);
        }
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
            machine_id: crate::paths::load_or_create_machine_id().ok(),
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
        updating: bool,
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
            software: Some(crate::software::heartbeat_software(cfg, updating)),
            hardware: Some(crate::hardware::heartbeat_hardware()),
            machine_id: crate::paths::load_or_create_machine_id().ok(),
            settings_ack_gen: self.take_settings_ack(),
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
        let parsed: HeartbeatResponse = read_json(resp).await?;
        if let Some(n) = parsed.host.settings_gen {
            self.note_settings_gen(n);
        }
        Ok(parsed)
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

    /// GET `{control_plane_url}/install/worker/bin/{platform}/{bin}` (or `.sha256`).
    /// Bins are only fetched from the paired origin. Missing sidecar is a hard fail.
    pub async fn fetch_install_bytes(&self, platform: &str, name: &str) -> Result<Vec<u8>> {
        let platform = safe_install_segment(platform)?;
        let name = safe_install_segment(name)?;
        let path = format!("/install/worker/bin/{platform}/{name}");
        let resp = self
            .http
            .get(self.url(&path))
            .timeout(Duration::from_secs(600))
            .send()
            .await
            .with_context(|| format!("GET {path}"))?;
        let status = resp.status();
        if status == StatusCode::NOT_FOUND {
            bail!("no prebuilt {name} for {platform}");
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("HTTP {status}: {}", body.trim());
        }
        let bytes = resp.bytes().await.context("read install body")?;
        Ok(bytes.to_vec())
    }

    pub async fn fetch_install_sha256(&self, platform: &str, name: &str) -> Result<String> {
        let sidecar = format!("{name}.sha256");
        let bytes = self.fetch_install_bytes(platform, &sidecar).await?;
        parse_sha256_sidecar(&bytes)
            .ok_or_else(|| anyhow::anyhow!("invalid sha256 sidecar for {name}"))
    }
}

pub(crate) fn parse_sha256_sidecar(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(bytes);
    let token = text.split_whitespace().next()?.trim();
    if token.len() != 64 || !token.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(token.to_ascii_lowercase())
}

pub(crate) fn safe_install_segment(s: &str) -> Result<&str> {
    let s = s.trim();
    if s.is_empty() || s.contains('/') || s.contains('\\') || s.contains("..") {
        bail!("invalid install path segment");
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    {
        bail!("invalid install path segment");
    }
    Ok(s)
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
    #[serde(skip_serializing_if = "Option::is_none")]
    machine_id: Option<uuid::Uuid>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    hardware: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    machine_id: Option<uuid::Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    settings_ack_gen: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HeartbeatResponse {
    #[serde(default)]
    pub host: HostPublic,
    #[serde(default)]
    pub effective: EffectiveModes,
    /// Served dist for this host's platform. `null` / omitted = no prebuilt.
    #[serde(default)]
    pub desired: Option<DesiredSoftware>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct DesiredSoftware {
    #[serde(default)]
    pub git_sha: String,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub bins: HashMap<String, DesiredBin>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DesiredBin {
    pub sha256: String,
    #[serde(default)]
    pub size: Option<u64>,
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
    /// Plane allows this host to self-update. Missing (old CP) defaults true.
    #[serde(default)]
    pub auto_update: Option<bool>,
    /// One-shot Update click. Host applies once even when auto is off.
    #[serde(default)]
    pub update_requested_at: Option<String>,
    #[serde(default)]
    pub settings_gen: Option<i32>,
}

impl HostPublic {
    pub fn auto_update_or_default(&self) -> bool {
        self.auto_update.unwrap_or(true)
    }

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_parses_desired_and_auto_update() {
        let raw = r#"{
            "host": { "auto_update": true, "paused": false, "drain": false },
            "effective": { "host": true, "container": false, "max_concurrent": 4 },
            "desired": {
                "git_sha": "abc123",
                "platform": "windows-x86_64",
                "bins": {
                    "fleet-host.exe": { "sha256": "aa", "size": 1 },
                    "fleet-worker.exe": { "sha256": "bb", "size": 2 },
                    "fleet-driver.exe": { "sha256": "cc", "size": 3 }
                }
            }
        }"#;
        let resp: HeartbeatResponse = serde_json::from_str(raw).expect("parse");
        assert!(resp.host.auto_update_or_default());
        let d = resp.desired.expect("desired");
        assert_eq!(d.git_sha, "abc123");
        assert_eq!(d.platform, "windows-x86_64");
        assert_eq!(d.bins["fleet-host.exe"].size, Some(1));
    }

    #[test]
    fn heartbeat_without_desired_is_null() {
        let resp: HeartbeatResponse =
            serde_json::from_str(r#"{"host":{},"effective":{}}"#).expect("parse");
        assert!(resp.desired.is_none());
        assert!(resp.host.auto_update_or_default());
        assert!(resp.host.update_requested_at.is_none());
    }

    #[test]
    fn parse_sha256_sidecar_takes_first_token() {
        assert_eq!(
            parse_sha256_sidecar(
                b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  fleet-host\n"
            )
            .as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        assert!(parse_sha256_sidecar(b"not-a-hash").is_none());
    }
}
