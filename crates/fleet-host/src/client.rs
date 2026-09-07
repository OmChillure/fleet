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

use agent_fleet_contracts::IsolationClass;

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
        if n > 0 { Some(n) } else { None }
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

    /// Cluster / compose bootstrap. Same response as pair. 409 `no_workspace`
    /// means retry after a user creates a workspace.
    pub async fn bootstrap(&self, cfg: &LocalConfig, secret: &str) -> Result<PairResponse> {
        let body = BootstrapRequest {
            hostname: hostname(),
            platform: detect_platform(),
            advertised_host: advertised_host(cfg),
            advertised_container: advertised_container(cfg),
            machine_id: crate::paths::load_or_create_machine_id().ok(),
        };
        let resp = self
            .http
            .post(self.url("/api/worker-hosts/bootstrap"))
            .bearer_auth(secret.trim())
            .json(&body)
            .send()
            .await
            .context("POST /api/worker-hosts/bootstrap")?;
        let status = resp.status();
        let bytes = resp.bytes().await.context("read bootstrap body")?;
        if status.as_u16() == 409 {
            let body = String::from_utf8_lossy(&bytes);
            if body.contains("no_workspace") {
                return Err(BootstrapPending.into());
            }
        }
        if !status.is_success() {
            let body = String::from_utf8_lossy(&bytes);
            bail!("HTTP {status}: {}", body.trim());
        }
        serde_json::from_slice(&bytes).with_context(|| {
            format!(
                "decode bootstrap JSON ({status}): {}",
                String::from_utf8_lossy(&bytes)
            )
        })
    }

    pub async fn heartbeat(
        &self,
        cfg: &LocalConfig,
        current_jobs: u32,
        error: Option<&str>,
        updating: bool,
        install_ack: Option<InstallAck>,
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
            agents: Some(crate::agents::heartbeat_agents()),
            hardware: Some(crate::hardware::heartbeat_hardware()),
            machine_id: crate::paths::load_or_create_machine_id().ok(),
            settings_ack_gen: self.take_settings_ack(),
            install_ack,
            runtime: Some(cfg.runtime_snapshot()),
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
        result: Option<serde_json::Value>,
    ) -> Result<()> {
        let token = self.bearer()?;
        let body = CompleteRequest {
            lease_token: lease_token.to_string(),
            ok,
            error: error.map(|s| s.to_string()),
            result,
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

#[derive(Debug, Error)]
#[error("no_workspace")]
pub struct BootstrapPending;

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

#[derive(Debug, Serialize)]
struct BootstrapRequest {
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
    agents: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hardware: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    machine_id: Option<uuid::Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    settings_ack_gen: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    install_ack: Option<InstallAck>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstallAck {
    pub spec: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
    #[serde(default)]
    pub install: Option<serde_json::Value>,
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
    #[serde(default)]
    pub version: Option<String>,
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
    #[serde(default)]
    pub runtime_settings: Option<serde_json::Value>,
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
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum JobRuntime {
    #[default]
    Host,
    Container,
    /// Guest kernel. Host maps this to Firecracker / QEMU / KubeVirt.
    /// Missing hypervisor → do not advertise; never silently run as Host.
    Vm,
}

impl JobRuntime {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Container => "container",
            Self::Vm => "vm",
        }
    }

    pub fn from_isolation(class: IsolationClass) -> Self {
        match class {
            IsolationClass::Process => Self::Host,
            IsolationClass::Container => Self::Container,
            IsolationClass::Vm => Self::Vm,
        }
    }

    pub fn isolation(self) -> IsolationClass {
        match self {
            Self::Host => IsolationClass::Process,
            Self::Container => IsolationClass::Container,
            Self::Vm => IsolationClass::Vm,
        }
    }

    /// Parse an isolation class string. `k8s` / `kubernetes` are placement, not isolation.
    pub fn parse_isolation(s: &str) -> Result<Self> {
        let trimmed = s.trim();
        match IsolationClass::parse(trimmed) {
            Some(class) => Ok(Self::from_isolation(class)),
            None => {
                if trimmed.eq_ignore_ascii_case("k8s") || trimmed.eq_ignore_ascii_case("kubernetes")
                {
                    bail!("k8s is placement, not isolation (use container)");
                }
                bail!("unknown isolation {trimmed:?}");
            }
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
    #[serde(default)]
    pub runtime: JobRuntime,
    /// Isolation class (`process` | `container` | `vm`). When set, maps onto `runtime`.
    /// Placement words (`k8s`) are rejected — a pod is still container isolation.
    #[serde(default)]
    pub isolation: Option<String>,
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
    pub persona_md: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub privileged: bool,
}

impl SpawnPayload {
    /// Isolation string wins when present; otherwise `runtime`.
    pub fn resolved_runtime(&self) -> Result<JobRuntime> {
        if let Some(raw) = self
            .isolation
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            JobRuntime::parse_isolation(raw)
        } else {
            Ok(self.runtime)
        }
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::LocalConfig;
    use crate::software::{package_bin_file_name, sha256_hex};
    use crate::update::{ApplyOptions, ApplyOutcome, UpdateState, apply_desired};
    use std::collections::{HashMap, HashSet};
    use std::path::Path;
    use tokio::sync::Mutex as AsyncMutex;

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
    fn isolation_parse_maps_to_runtime() {
        assert_eq!(
            JobRuntime::parse_isolation("process").unwrap(),
            JobRuntime::Host
        );
        assert_eq!(
            JobRuntime::parse_isolation("host").unwrap(),
            JobRuntime::Host
        );
        assert_eq!(
            JobRuntime::parse_isolation("native").unwrap(),
            JobRuntime::Host
        );
        assert_eq!(
            JobRuntime::parse_isolation("container").unwrap(),
            JobRuntime::Container
        );
        assert_eq!(
            JobRuntime::parse_isolation("docker").unwrap(),
            JobRuntime::Container
        );
        assert_eq!(
            JobRuntime::parse_isolation("pod").unwrap(),
            JobRuntime::Container
        );
        assert_eq!(JobRuntime::parse_isolation("vm").unwrap(), JobRuntime::Vm);
        assert_eq!(
            JobRuntime::parse_isolation("firecracker").unwrap(),
            JobRuntime::Vm
        );
        assert_eq!(JobRuntime::parse_isolation("qemu").unwrap(), JobRuntime::Vm);
        assert_eq!(
            JobRuntime::from_isolation(IsolationClass::Process),
            JobRuntime::Host
        );
        assert_eq!(
            JobRuntime::from_isolation(IsolationClass::Container),
            JobRuntime::Container
        );
        assert_eq!(
            JobRuntime::from_isolation(IsolationClass::Vm),
            JobRuntime::Vm
        );
        assert_eq!(JobRuntime::Host.isolation(), IsolationClass::Process);
        assert_eq!(JobRuntime::Container.isolation(), IsolationClass::Container);
        assert_eq!(JobRuntime::Vm.isolation(), IsolationClass::Vm);
    }

    #[test]
    fn isolation_parse_rejects_k8s_placement() {
        let k8s = JobRuntime::parse_isolation("k8s").unwrap_err().to_string();
        assert!(k8s.contains("placement"), "{k8s}");
        assert!(JobRuntime::parse_isolation("kubernetes").is_err());
        assert!(JobRuntime::parse_isolation("nonsense").is_err());
        assert!(JobRuntime::parse_isolation("").is_err());
    }

    #[test]
    fn payload_isolation_overrides_runtime() {
        let raw = r#"{"runtime":"host","isolation":"vm"}"#;
        let p: SpawnPayload = serde_json::from_str(raw).expect("parse");
        assert_eq!(p.runtime, JobRuntime::Host);
        assert_eq!(p.resolved_runtime().unwrap(), JobRuntime::Vm);
        let container: SpawnPayload =
            serde_json::from_str(r#"{"runtime":"host","isolation":"container"}"#).unwrap();
        assert_eq!(container.resolved_runtime().unwrap(), JobRuntime::Container);
        let process: SpawnPayload = serde_json::from_str(r#"{"isolation":"process"}"#).unwrap();
        assert_eq!(process.resolved_runtime().unwrap(), JobRuntime::Host);
        let k8s: SpawnPayload =
            serde_json::from_str(r#"{"runtime":"container","isolation":"k8s"}"#).unwrap();
        assert!(
            k8s.resolved_runtime()
                .unwrap_err()
                .to_string()
                .contains("placement")
        );
    }

    #[test]
    fn parse_sha256_sidecar_takes_first_token() {
        const HASH: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert_eq!(
            parse_sha256_sidecar(format!("{HASH}  fleet-host\n").as_bytes()).as_deref(),
            Some(HASH)
        );
        // GNU binary-mode (`hash *name`) and bare hash still yield the file SHA.
        assert_eq!(
            parse_sha256_sidecar(format!("{HASH} *fleet-host\n").as_bytes()).as_deref(),
            Some(HASH)
        );
        assert_eq!(parse_sha256_sidecar(HASH.as_bytes()).as_deref(), Some(HASH));
        assert_eq!(
            parse_sha256_sidecar(format!("  {HASH}\r\n").as_bytes()).as_deref(),
            Some(HASH)
        );
        assert_eq!(
            parse_sha256_sidecar(HASH.to_ascii_uppercase().as_bytes()).as_deref(),
            Some(HASH)
        );
        assert!(parse_sha256_sidecar(b"not-a-hash").is_none());
    }

    /// Sidecar is a 64-hex *file* hash. Git bake / tree-ids must fail closed
    /// so they cannot be compared as if they identified the bin.
    #[test]
    fn parse_sha256_sidecar_rejects_bake_id() {
        assert!(parse_sha256_sidecar(b"").is_none());
        assert!(parse_sha256_sidecar(b"   \n").is_none());
        assert!(parse_sha256_sidecar(b"abc123").is_none());
        assert!(parse_sha256_sidecar(b"deadbeef").is_none());
        // Typical git SHAs (7 / 12 / 40 hex) are bake ids, not install SHAs.
        assert!(parse_sha256_sidecar(b"a1b2c3d").is_none());
        assert!(parse_sha256_sidecar(b"0123456789ab").is_none());
        assert!(parse_sha256_sidecar(b"0123456789abcdef0123456789abcdef01234567").is_none());
        assert!(parse_sha256_sidecar(b"abc1234-dirty").is_none());
        // Wrong length even when every char is hex.
        assert!(
            parse_sha256_sidecar(
                b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015a"
            )
            .is_none()
        );
        assert!(
            parse_sha256_sidecar(
                b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ada"
            )
            .is_none()
        );
        assert!(
            parse_sha256_sidecar(
                b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015adX"
            )
            .is_none()
        );
        // Manifest JSON is not a sidecar; do not scrape a git_sha out of it.
        assert!(
            parse_sha256_sidecar(br#"{"git_sha":"abc1234","bins":{}}"#).is_none(),
            "sidecar must not be treated as bake-id JSON"
        );
    }

    struct MockInstall {
        base: String,
        log: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
        _join: std::thread::JoinHandle<()>,
    }

    fn spawn_install_mock(files: HashMap<String, Vec<u8>>) -> MockInstall {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log2 = log.clone();
        let files = std::sync::Arc::new(files);
        let join = std::thread::spawn(move || {
            listener.set_nonblocking(false).ok();
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let mut buf = vec![0u8; 16384];
                let n = match s.read(&mut buf) {
                    Ok(n) => n,
                    Err(_) => continue,
                };
                let req = String::from_utf8_lossy(&buf[..n]);
                let first = req.lines().next().unwrap_or("");
                let mut it = first.split_whitespace();
                let method = it.next().unwrap_or("").to_string();
                let path = it.next().unwrap_or("").to_string();
                log2.lock().unwrap().push((method, path.clone()));
                let (status, payload) = match files.get(&path) {
                    Some(b) => ("200 OK", b.clone()),
                    None => ("404 Not Found", Vec::new()),
                };
                let header = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                let _ = s.write_all(header.as_bytes());
                let _ = s.write_all(&payload);
                let _ = s.flush();
            }
        });
        MockInstall {
            base: format!("http://{addr}"),
            log,
            _join: join,
        }
    }

    fn paths_hit(mock: &MockInstall) -> Vec<String> {
        mock.log
            .lock()
            .unwrap()
            .iter()
            .map(|(_, p)| p.clone())
            .collect()
    }

    fn write_bin(dir: &Path, name: &str, bytes: &[u8]) {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&p).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&p, perms).unwrap();
        }
    }

    fn bin_name(logical: &str, platform: &str) -> String {
        package_bin_file_name(logical, platform.starts_with("windows-"))
    }

    fn desired_for(platform: &str, bins: &[(&str, &[u8])]) -> DesiredSoftware {
        let mut map = HashMap::new();
        for (name, bytes) in bins {
            map.insert(
                (*name).to_string(),
                DesiredBin {
                    sha256: sha256_hex(bytes),
                    size: Some(bytes.len() as u64),
                    version: None,
                },
            );
        }
        DesiredSoftware {
            git_sha: "deadbeef".into(),
            platform: platform.into(),
            bins: map,
        }
    }

    fn dry_apply_opts() -> ApplyOptions {
        ApplyOptions {
            windows_self_replace: false,
            dry_restart: true,
        }
    }

    async fn apply_dir(
        client: &HostClient,
        dest: &Path,
        desired: &DesiredSoftware,
    ) -> Result<ApplyOutcome> {
        apply_desired(
            client,
            &LocalConfig::default(),
            desired,
            dest,
            &AsyncMutex::new(HashSet::new()),
            &UpdateState::new(),
            &dry_apply_opts(),
        )
        .await
    }

    fn install_path(platform: &str, name: &str) -> String {
        format!("/install/worker/bin/{platform}/{name}")
    }

    /// Product path (`apply_desired` → `download_changed`): local file SHA ==
    /// desired/sidecar SHA must not GET that bin. A mismatched child must GET
    /// `{bin}.sha256` then `{bin}`. Sidecar is a 64-hex file hash, not a bake id.
    #[tokio::test]
    async fn sidecar_match_reuses_bins_mismatch_refetches() {
        let platform = detect_platform();
        let host_name = bin_name("fleet-host", &platform);
        let driver_name = bin_name("fleet-driver", &platform);
        let host = b"host-same";
        let old_d = b"driver-old";
        let new_d = b"driver-new";
        let dir = tempfile::tempdir().unwrap();
        write_bin(dir.path(), &host_name, host);
        write_bin(dir.path(), &driver_name, old_d);

        let host_sha = sha256_hex(host);
        let driver_sha = sha256_hex(new_d);
        let mock = spawn_install_mock(HashMap::from([
            (
                format!("{}.sha256", install_path(&platform, &host_name)),
                format!("{host_sha}  {host_name}\n").into_bytes(),
            ),
            (
                install_path(&platform, &host_name),
                b"must-not-be-fetched-host".to_vec(),
            ),
            (
                format!("{}.sha256", install_path(&platform, &driver_name)),
                format!("{driver_sha}  {driver_name}\n").into_bytes(),
            ),
            (install_path(&platform, &driver_name), new_d.to_vec()),
        ]));
        let client = HostClient::for_pair(&mock.base, &LocalConfig::default()).unwrap();
        let mut desired = desired_for(
            &platform,
            &[
                (host_name.as_str(), host.as_slice()),
                (driver_name.as_str(), new_d.as_slice()),
            ],
        );
        // Hex case on the matching desired SHA must still reuse the on-disk host.
        desired.bins.get_mut(&host_name).unwrap().sha256 = host_sha.to_ascii_uppercase();

        let out = apply_dir(&client, dir.path(), &desired)
            .await
            .expect("driver mismatch applies; matching host is reuse");
        assert_eq!(out, ApplyOutcome::ChildrenSwapped);
        assert_eq!(
            std::fs::read(dir.path().join(&host_name)).unwrap(),
            host,
            "matching local SHA must keep the on-disk host bin"
        );
        assert_eq!(std::fs::read(dir.path().join(&driver_name)).unwrap(), new_d);

        let paths = paths_hit(&mock);
        let host_bin = install_path(&platform, &host_name);
        let driver_side = format!("{}.sha256", install_path(&platform, &driver_name));
        let driver_bin = install_path(&platform, &driver_name);
        assert!(
            !paths.iter().any(|p| p == &host_bin),
            "local file SHA == desired/sidecar SHA must not GET the host bin, got {paths:?}"
        );
        let side_i = paths.iter().position(|p| p == &driver_side);
        let bin_i = paths.iter().position(|p| p == &driver_bin);
        assert!(
            side_i.is_some() && bin_i.is_some_and(|b| b > side_i.unwrap()),
            "mismatch must GET {{bin}}.sha256 then {{bin}}, got {paths:?}"
        );
    }

    #[tokio::test]
    async fn fetch_install_sha256_sidecar_match_reuses_bins() {
        let platform = detect_platform();
        let host_name = bin_name("fleet-host", &platform);
        let driver_name = bin_name("fleet-driver", &platform);
        let host = b"host-keep";
        let old_d = b"driver-v1";
        let new_d = b"driver-v2";
        let dir = tempfile::tempdir().unwrap();
        write_bin(dir.path(), &host_name, host);
        write_bin(dir.path(), &driver_name, old_d);

        let mock = spawn_install_mock(HashMap::from([
            (
                format!("{}.sha256", install_path(&platform, &host_name)),
                format!("{}  {host_name}\n", sha256_hex(host)).into_bytes(),
            ),
            (
                install_path(&platform, &host_name),
                b"must-not-be-fetched".to_vec(),
            ),
            (
                format!("{}.sha256", install_path(&platform, &driver_name)),
                format!("{}  {driver_name}\n", sha256_hex(new_d)).into_bytes(),
            ),
            (install_path(&platform, &driver_name), new_d.to_vec()),
        ]));
        let client = HostClient::for_pair(&mock.base, &LocalConfig::default()).unwrap();
        let desired = desired_for(
            &platform,
            &[
                (host_name.as_str(), host.as_slice()),
                (driver_name.as_str(), new_d.as_slice()),
            ],
        );
        apply_dir(&client, dir.path(), &desired)
            .await
            .expect("match is reuse, not an error");
        let paths = paths_hit(&mock);
        assert!(
            !paths
                .iter()
                .any(|p| p == &install_path(&platform, &host_name)),
            "reuse must not GET the matching bin, got {paths:?}"
        );
        assert_eq!(std::fs::read(dir.path().join(&host_name)).unwrap(), host);
    }

    #[tokio::test]
    async fn fetch_install_sha256_sidecar_mismatch_refetches() {
        let platform = detect_platform();
        let host_name = bin_name("fleet-host", &platform);
        let driver_name = bin_name("fleet-driver", &platform);
        let host = b"host-keep";
        let old_d = b"stale-driver";
        let new_d = b"fresh-driver";
        let dir = tempfile::tempdir().unwrap();
        write_bin(dir.path(), &host_name, host);
        write_bin(dir.path(), &driver_name, old_d);

        let mock = spawn_install_mock(HashMap::from([
            (
                format!("{}.sha256", install_path(&platform, &driver_name)),
                format!("{}  {driver_name}\n", sha256_hex(new_d)).into_bytes(),
            ),
            (install_path(&platform, &driver_name), new_d.to_vec()),
        ]));
        let client = HostClient::for_pair(&mock.base, &LocalConfig::default()).unwrap();
        let desired = desired_for(
            &platform,
            &[
                (host_name.as_str(), host.as_slice()),
                (driver_name.as_str(), new_d.as_slice()),
            ],
        );
        let out = apply_dir(&client, dir.path(), &desired)
            .await
            .expect("mismatch refetches");
        assert_eq!(out, ApplyOutcome::ChildrenSwapped);
        assert_eq!(std::fs::read(dir.path().join(&driver_name)).unwrap(), new_d);
        let paths = paths_hit(&mock);
        assert_eq!(
            paths,
            vec![
                format!("{}.sha256", install_path(&platform, &driver_name)),
                install_path(&platform, &driver_name),
            ],
            "mismatch must GET sidecar then bin, got {paths:?}"
        );
    }

    #[tokio::test]
    async fn fetch_install_sha256_invalid_sidecar_fails_closed() {
        let platform = detect_platform();
        let host_name = bin_name("fleet-host", &platform);
        let driver_name = bin_name("fleet-driver", &platform);
        let host = b"host-same";
        let old_d = b"driver-old";
        let new_d = b"driver-new";
        for (label, body) in [
            ("garbage", b"not-a-hash\n".as_slice()),
            ("bake-id-7", b"abc1234\n".as_slice()),
            ("bake-id-12", b"0123456789ab\n".as_slice()),
            (
                "git-sha-40",
                b"0123456789abcdef0123456789abcdef01234567\n".as_slice(),
            ),
            ("manifest-json", br#"{"git_sha":"abc1234"}"#.as_slice()),
        ] {
            let dir = tempfile::tempdir().unwrap();
            write_bin(dir.path(), &host_name, host);
            write_bin(dir.path(), &driver_name, old_d);
            let mock = spawn_install_mock(HashMap::from([
                (
                    format!("{}.sha256", install_path(&platform, &driver_name)),
                    body.to_vec(),
                ),
                (
                    install_path(&platform, &driver_name),
                    b"must-not-be-fetched".to_vec(),
                ),
            ]));
            let client = HostClient::for_pair(&mock.base, &LocalConfig::default()).unwrap();
            let desired = desired_for(
                &platform,
                &[
                    (host_name.as_str(), host.as_slice()),
                    (driver_name.as_str(), new_d.as_slice()),
                ],
            );
            let err = apply_dir(&client, dir.path(), &desired)
                .await
                .expect_err(label);
            let msg = err.to_string();
            assert!(
                msg.contains("invalid sha256 sidecar"),
                "{label}: expected fail-closed, got {msg}"
            );
            let paths = paths_hit(&mock);
            assert_eq!(
                paths,
                vec![format!("{}.sha256", install_path(&platform, &driver_name))],
                "{label}: invalid/bake-id sidecar must not GET the bin, got {paths:?}"
            );
            assert_eq!(
                std::fs::read(dir.path().join(&driver_name)).unwrap(),
                old_d,
                "{label}: on-disk bin must stay put"
            );
        }
    }

    #[tokio::test]
    async fn fetch_install_sha256_missing_sidecar_fails_closed() {
        let platform = detect_platform();
        let name = bin_name("fleet-host", &platform);
        let mock = spawn_install_mock(HashMap::from([(
            install_path(&platform, &name),
            b"bin-without-sidecar".to_vec(),
        )]));
        let client = HostClient::for_pair(&mock.base, &LocalConfig::default()).unwrap();
        let err = client
            .fetch_install_sha256(&platform, &name)
            .await
            .expect_err("missing sidecar");
        let msg = err.to_string();
        assert!(
            msg.contains("no prebuilt") || msg.contains("invalid sha256 sidecar"),
            "missing sidecar must fail closed, got {msg}"
        );
        let paths = paths_hit(&mock);
        assert_eq!(
            paths,
            vec![format!("{}.sha256", install_path(&platform, &name))],
            "must request the sidecar, not fall back to hashing the bin: {paths:?}"
        );
    }
}
