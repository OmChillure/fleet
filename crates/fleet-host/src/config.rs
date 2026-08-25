//! Worker-local settings (`~/.config/fleet-host/config.toml`).
//! Never accepted from the control plane.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::paths::{self, config_dir, config_path, expand_tilde, token_path};

const WORKER_TOKEN_KEY: &str = "FLEET_WORKER_TOKEN";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ContainerBackend {
    #[default]
    K3d,
    Docker,
}

impl ContainerBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::K3d => "k3d",
            Self::Docker => "docker",
        }
    }

    /// Binary that must be on PATH to advertise this backend.
    pub fn required_binary(self) -> &'static str {
        self.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LocalConfig {
    #[serde(default = "default_true")]
    pub run_host: bool,
    #[serde(default)]
    pub run_container: bool,
    #[serde(default)]
    pub container_backend: ContainerBackend,
    #[serde(default = "paths::default_work_dir_string")]
    pub work_dir: String,
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
    #[serde(default = "default_k3d_cluster")]
    pub k3d_cluster: String,
    #[serde(default = "default_k3d_memory")]
    pub k3d_memory: String,
    #[serde(default)]
    pub k3d_agents: u32,
    #[serde(default = "default_docker_image")]
    pub docker_image: String,
    #[serde(default = "default_docker_network")]
    pub docker_network: String,
    #[serde(default)]
    pub allow_privileged: bool,
    #[serde(default = "default_true")]
    pub use_host_git_credentials: bool,
    /// This box will apply CP-served updates. Intersected with CP `auto_update`.
    #[serde(default = "default_true")]
    pub auto_update: bool,
    #[serde(default = "default_driver_bin")]
    pub fleet_driver_bin: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default = "default_heartbeat")]
    pub heartbeat_interval_s: u64,
    #[serde(default = "default_poll")]
    pub poll_interval_s: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_proxy: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub extra_env: HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_plane_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_id: Option<Uuid>,
}

fn env_bool(name: &str) -> Option<bool> {
    let raw = std::env::var(name).ok()?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn default_true() -> bool {
    true
}
fn default_max_concurrent() -> u32 {
    8
}
fn default_k3d_cluster() -> String {
    "agent-fleet".into()
}
fn default_k3d_memory() -> String {
    "4g".into()
}
fn default_docker_image() -> String {
    "agent-fleet-api:local".into()
}
fn default_docker_network() -> String {
    "bridge".into()
}
fn default_driver_bin() -> String {
    "fleet-driver".into()
}
fn default_log_level() -> String {
    "info".into()
}
fn default_heartbeat() -> u64 {
    10
}
fn default_poll() -> u64 {
    2
}

impl Default for LocalConfig {
    fn default() -> Self {
        Self {
            run_host: true,
            run_container: false,
            container_backend: ContainerBackend::K3d,
            work_dir: paths::default_work_dir_string(),
            max_concurrent: 8,
            k3d_cluster: default_k3d_cluster(),
            k3d_memory: default_k3d_memory(),
            k3d_agents: 0,
            docker_image: default_docker_image(),
            docker_network: default_docker_network(),
            allow_privileged: false,
            use_host_git_credentials: true,
            auto_update: true,
            fleet_driver_bin: default_driver_bin(),
            log_level: default_log_level(),
            heartbeat_interval_s: 10,
            poll_interval_s: 2,
            http_proxy: None,
            extra_env: HashMap::new(),
            control_plane_url: None,
            host_id: None,
        }
    }
}

impl LocalConfig {
    pub fn defaults() -> Self {
        Self::default()
    }

    pub fn load_or_default() -> Result<Self> {
        let path = config_path();
        let mut cfg = if path.is_file() {
            Self::load_path(&path)?
        } else {
            Self::default()
        };
        cfg.apply_env();
        Ok(cfg)
    }

    /// Overlay process env. Used by compose/kind (`FLEET_HOST_*`).
    pub fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("FLEET_CONTROL_PLANE_URL") {
            let t = v.trim();
            if !t.is_empty() {
                self.control_plane_url = Some(t.to_string());
            }
        }
        if let Some(v) = env_bool("FLEET_HOST_RUN_HOST") {
            self.run_host = v;
        }
        if let Some(v) = env_bool("FLEET_HOST_RUN_CONTAINER") {
            self.run_container = v;
        }
        if let Ok(v) = std::env::var("K8S_WORKER_IMAGE") {
            let t = v.trim();
            if !t.is_empty() {
                self.docker_image = t.to_string();
            }
        }
    }

    pub fn load_path(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("parse {}", path.display()))
    }

    pub fn save(&self) -> Result<PathBuf> {
        let dir = config_dir();
        ensure_dir_mode(&dir, 0o700)?;
        let path = config_path();
        let body = toml::to_string_pretty(self).context("serialize config.toml")?;
        write_file_mode(&path, body.as_bytes(), 0o600)?;
        Ok(path)
    }

    pub fn work_dir_path(&self) -> PathBuf {
        expand_tilde(&self.work_dir)
    }

    pub fn session_dir(&self, session_id: Uuid) -> PathBuf {
        self.work_dir_path().join(session_id.to_string())
    }

    pub fn heartbeat_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.heartbeat_interval_s.max(1))
    }

    pub fn poll_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.poll_interval_s.max(1))
    }

    pub fn local_max_jobs(&self) -> u32 {
        self.max_concurrent.clamp(1, 256)
    }
}

pub fn read_machine_token() -> Result<Option<String>> {
    let path = token_path();
    if !path.is_file() {
        return Ok(None);
    }
    let mut s = String::new();
    fs::File::open(&path)
        .with_context(|| format!("read {}", path.display()))?
        .read_to_string(&mut s)?;
    let t = s.trim();
    if t.is_empty() {
        Ok(None)
    } else {
        Ok(Some(t.to_string()))
    }
}

pub fn write_machine_token(token: &str) -> Result<PathBuf> {
    let dir = config_dir();
    ensure_dir_mode(&dir, 0o700)?;
    let path = token_path();
    let mut body = token.trim().as_bytes().to_vec();
    body.push(b'\n');
    write_file_mode(&path, &body, 0o600)?;
    Ok(path)
}

pub fn delete_machine_token() -> Result<bool> {
    let path = token_path();
    if !path.exists() {
        return Ok(false);
    }
    fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
    Ok(true)
}

/// Overlay `extra_env` onto `payload_env`. `FLEET_WORKER_TOKEN` in extra is ignored.
pub fn merge_child_env(
    payload_env: &HashMap<String, String>,
    extra_env: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut env = payload_env.clone();
    for (k, v) in extra_env {
        if k == WORKER_TOKEN_KEY {
            continue;
        }
        env.insert(k.clone(), v.clone());
    }
    env
}

pub fn ensure_dir_mode(dir: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        if !dir.exists() {
            let mut b = fs::DirBuilder::new();
            b.recursive(true);
            b.mode(mode);
            b.create(dir)
                .with_context(|| format!("mkdir {}", dir.display()))?;
        }
        let mut perms = fs::metadata(dir)
            .with_context(|| format!("stat {}", dir.display()))?
            .permissions();
        perms.set_mode(mode);
        if let Err(e) = fs::set_permissions(dir, perms) {
            // In-cluster PVC is often root-owned (fsGroup is group-only).
            // Keep going if we can still create files there.
            let probe = dir.join(".fleet-host-writable");
            match fs::OpenOptions::new().create(true).write(true).open(&probe) {
                Ok(_) => {
                    let _ = fs::remove_file(&probe);
                    tracing::warn!(
                        path = %dir.display(),
                        error = %e,
                        "could not chmod config dir; directory is writable"
                    );
                }
                Err(w) => {
                    return Err(e).with_context(|| {
                        format!("chmod {:o} {} (also not writable: {w})", mode, dir.display())
                    });
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        fs::create_dir_all(dir).with_context(|| format!("mkdir {}", dir.display()))?;
    }
    Ok(())
}

fn write_file_mode(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut opts = OpenOptions::new();
        opts.write(true).create(true).truncate(true).mode(mode);
        let mut f = opts
            .open(path)
            .with_context(|| format!("open {}", path.display()))?;
        f.write_all(bytes)
            .with_context(|| format!("write {}", path.display()))?;
        f.flush()?;
        let mut perms = f
            .metadata()
            .with_context(|| format!("stat {}", path.display()))?
            .permissions();
        perms.set_mode(mode);
        fs::set_permissions(path, perms)
            .with_context(|| format!("chmod {:o} {}", mode, path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("open {}", path.display()))?;
        f.write_all(bytes)
            .with_context(|| format!("write {}", path.display()))?;
        f.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_toml_roundtrip() {
        let mut cfg = LocalConfig::defaults();
        cfg.run_container = true;
        cfg.container_backend = ContainerBackend::Docker;
        cfg.max_concurrent = 4;
        cfg.k3d_cluster = "lab".into();
        cfg.extra_env
            .insert("HTTP_PROXY".into(), "http://127.0.0.1:9".into());
        cfg.control_plane_url = Some("http://127.0.0.1:8080".into());
        cfg.host_id = Some(Uuid::nil());
        let s = toml::to_string_pretty(&cfg).expect("serialize");
        let back: LocalConfig = toml::from_str(&s).expect("deserialize");
        assert_eq!(cfg, back);
        assert!(s.contains("run_host"));
        assert!(s.contains("container_backend"));
    }

    #[test]
    fn docker_network_defaults_to_bridge() {
        let cfg = LocalConfig::defaults();
        assert_eq!(cfg.docker_network, "bridge");
        let parsed: LocalConfig = toml::from_str("").expect("empty toml");
        assert_eq!(parsed.docker_network, "bridge");
    }

    #[test]
    fn auto_update_defaults_true_and_can_be_disabled() {
        let cfg = LocalConfig::defaults();
        assert!(cfg.auto_update);
        let parsed: LocalConfig = toml::from_str("").expect("empty toml");
        assert!(
            parsed.auto_update,
            "missing auto_update must default true: operator opt-out only"
        );
        let off: LocalConfig = toml::from_str("auto_update = false").expect("toml");
        assert!(!off.auto_update);
    }

    #[test]
    fn extra_env_cannot_replace_fleet_worker_token() {
        let mut payload = HashMap::new();
        payload.insert(WORKER_TOKEN_KEY.into(), "real-token".into());
        payload.insert("FOO".into(), "from-payload".into());
        let mut extra = HashMap::new();
        extra.insert(WORKER_TOKEN_KEY.into(), "evil".into());
        extra.insert("FOO".into(), "from-extra".into());
        extra.insert("BAR".into(), "extra-only".into());
        let merged = merge_child_env(&payload, &extra);
        assert_eq!(
            merged.get(WORKER_TOKEN_KEY).map(String::as_str),
            Some("real-token")
        );
        assert_eq!(merged.get("FOO").map(String::as_str), Some("from-extra"));
        assert_eq!(merged.get("BAR").map(String::as_str), Some("extra-only"));
        // extra must not introduce a token when payload omitted it either
        let merged2 = merge_child_env(&HashMap::new(), &extra);
        assert!(!merged2.contains_key(WORKER_TOKEN_KEY));
        assert_eq!(merged2.get("BAR").map(String::as_str), Some("extra-only"));
    }
}
