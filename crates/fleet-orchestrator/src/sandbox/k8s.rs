//! Kubernetes sandbox: create / observe / delete `FleetSandbox` CRs.
//!
//! The operator turns each CR into a Pod + NetworkPolicy. This adapter does
//! not exec on the API host and does not run the in-process agent loop.

use std::collections::BTreeMap;
use std::time::Duration;

use async_trait::async_trait;
use fleet_operator::crd::{
    dns_label, FleetSandbox, FleetSandboxSpec, FleetSandboxStatus, SandboxNetwork,
    SandboxResources, LABEL_PROJECT, LABEL_SANDBOX, LABEL_SESSION, LABEL_WORKSPACE,
};
use kube::api::{DeleteParams, ListParams, PostParams};
use kube::{Api, Client, Resource, ResourceExt};
use uuid::Uuid;

use super::{
    RuntimeHandle, SandboxError, SandboxRuntime, SandboxState, Signal, SpawnRequest,
};

/// etcd / CR env budget.
pub const MAX_SPEC_MD_BYTES: usize = 200 * 1024;

pub const DEFAULT_NAMESPACE: &str = "agent-fleet";
pub const DEFAULT_IMAGE: &str = "agent-fleet-api:local";
pub const DEFAULT_WORKER_API_URL: &str = "http://api.agent-fleet.svc.cluster.local:8080";
const DEFAULT_CPU: &str = "250m";
const DEFAULT_MEMORY: &str = "256Mi";
const DEFAULT_EPHEMERAL: &str = "1Gi";
const POD_WORK_DIR: &str = "/data/work";

#[derive(Debug, Clone)]
pub struct K8sOpts {
    pub namespace: String,
    pub image: String,
    pub command: Vec<String>,
    pub worker_api_url: String,
    pub kill_grace: Duration,
}

impl Default for K8sOpts {
    fn default() -> Self {
        Self {
            namespace: DEFAULT_NAMESPACE.into(),
            image: DEFAULT_IMAGE.into(),
            command: vec!["fleet-worker".into()],
            worker_api_url: DEFAULT_WORKER_API_URL.into(),
            kill_grace: Duration::from_secs(15),
        }
    }
}

impl K8sOpts {
    pub fn from_env() -> Self {
        let command = match std::env::var("K8S_WORKER_COMMAND") {
            Ok(s) if !s.trim().is_empty() => {
                s.split_whitespace().map(|p| p.to_string()).collect()
            }
            _ => vec!["fleet-worker".into()],
        };
        let kill_grace = std::env::var("KILL_GRACE_S")
            .ok()
            .and_then(|s| s.parse().ok())
            .map(Duration::from_secs)
            .unwrap_or_else(|| Duration::from_secs(15));
        Self {
            namespace: env_or("K8S_NAMESPACE", DEFAULT_NAMESPACE),
            image: env_or("K8S_WORKER_IMAGE", DEFAULT_IMAGE),
            command,
            worker_api_url: env_or("K8S_WORKER_API_URL", DEFAULT_WORKER_API_URL)
                .trim_end_matches('/')
                .to_string(),
            kill_grace,
        }
    }
}

fn env_or(k: &str, def: &str) -> String {
    std::env::var(k)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| def.to_string())
}

pub struct K8sRuntime {
    opts: K8sOpts,
}

impl K8sRuntime {
    pub fn new(opts: K8sOpts) -> Result<Self, SandboxError> {
        if let Some(msg) = loopback_worker_url_error(&opts.worker_api_url) {
            return Err(SandboxError::Spawn(msg));
        }
        if opts.namespace.trim().is_empty() {
            return Err(SandboxError::Spawn("K8S_NAMESPACE is empty".into()));
        }
        if opts.image.trim().is_empty() {
            return Err(SandboxError::Spawn("K8S_WORKER_IMAGE is empty".into()));
        }
        if opts.command.is_empty() || opts.command.iter().all(|s| s.trim().is_empty()) {
            return Err(SandboxError::Spawn("K8S_WORKER_COMMAND is empty".into()));
        }
        Ok(Self { opts })
    }

    pub fn from_env() -> Result<Self, SandboxError> {
        Self::new(K8sOpts::from_env())
    }

    async fn api(&self) -> Result<Api<FleetSandbox>, SandboxError> {
        let client = Client::try_default().await.map_err(|e| {
            SandboxError::Spawn(format!(
                "k8s client (in-cluster config or KUBECONFIG): {e}"
            ))
        })?;
        Ok(Api::namespaced(client, &self.opts.namespace))
    }
}

/// DNS-1123 CR name from a session id (pod will be `fsb-<name>`).
pub fn cr_name_for_session(session_id: Uuid) -> String {
    dns_label(&session_id.to_string())
}

pub fn loopback_worker_url_error(url: &str) -> Option<String> {
    let host = url_host(url)?;
    if is_loopback_host(&host) {
        Some(format!(
            "K8S_WORKER_API_URL {url} is loopback; pods cannot reach the API host. \
             Set a cluster-reachable URL (in-cluster Service DNS, or a host IPv4/IPv6 \
             literal — DNS names are not NetworkPolicy CIDRs)."
        ))
    } else {
        None
    }
}

fn url_host(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))?;
    let hostport = rest.split('/').next().unwrap_or(rest);
    let host = if hostport.starts_with('[') {
        hostport
            .trim_start_matches('[')
            .split(']')
            .next()
            .unwrap_or(hostport)
            .to_string()
    } else {
        hostport.split(':').next().unwrap_or(hostport).to_string()
    };
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().trim_matches(|c| c == '[' || c == ']');
    h.eq_ignore_ascii_case("localhost") || h == "127.0.0.1" || h == "::1"
}

pub fn spec_md_too_large(spec: &str) -> bool {
    spec.len() > MAX_SPEC_MD_BYTES
}

/// In-cluster Service DNS stays default-deny except kube-dns + fleet-api.
/// A host IP callback needs an allowlist CIDR or NetworkPolicy blocks heartbeats.
fn network_for_worker_url(url: &str) -> SandboxNetwork {
    match url_host(url) {
        Some(host) if is_ipv4_literal(&host) => SandboxNetwork {
            egress_mode: "allowlist".into(),
            allowlist: vec![format!("{host}/32")],
        },
        Some(host) if is_ipv6_literal(&host) => SandboxNetwork {
            egress_mode: "allowlist".into(),
            allowlist: vec![format!("{host}/128")],
        },
        _ => SandboxNetwork {
            egress_mode: "none".into(),
            allowlist: vec![],
        },
    }
}

fn is_ipv4_literal(host: &str) -> bool {
    let mut parts = host.split('.');
    let mut n = 0;
    for p in parts.by_ref() {
        n += 1;
        if n > 4 {
            return false;
        }
        if p.parse::<u8>().is_err() {
            return false;
        }
    }
    n == 4
}

fn is_ipv6_literal(host: &str) -> bool {
    host.contains(':') && host.parse::<std::net::Ipv6Addr>().is_ok()
}

pub fn spec_from_request(req: &SpawnRequest, opts: &K8sOpts) -> Result<FleetSandboxSpec, SandboxError> {
    if let Some(msg) = loopback_worker_url_error(&opts.worker_api_url) {
        return Err(SandboxError::Spawn(msg));
    }
    let spec_md = req.task_spec.as_deref().unwrap_or("");
    if spec_md_too_large(spec_md) {
        return Err(SandboxError::Spawn(format!(
            "task spec is {} bytes; k8s env limit is {MAX_SPEC_MD_BYTES}",
            spec_md.len()
        )));
    }

    let mut env = BTreeMap::new();
    for (k, v) in &req.env {
        if k.starts_with("FLEET_") {
            env.insert(k.clone(), v.clone());
        }
    }
    env.insert("FLEET_ORCHESTRATOR_URL".into(), opts.worker_api_url.clone());
    env.insert("FLEET_API_BASE".into(), opts.worker_api_url.clone());
    env.insert("FLEET_WORK_DIR".into(), POD_WORK_DIR.into());
    if !spec_md.is_empty() {
        env.insert("FLEET_SPEC_MD".into(), spec_md.to_string());
    }
    if let Some(t) = req.task_title.as_deref().filter(|s| !s.is_empty()) {
        env.insert("FLEET_TASK_TITLE".into(), t.to_string());
    }
    if let Some(s) = req.task_summary.as_deref().filter(|s| !s.is_empty()) {
        env.insert("FLEET_TASK_SUMMARY".into(), s.to_string());
    }

    let ttl = req.timeout.as_secs().max(1) as i64;
    Ok(FleetSandboxSpec {
        workspace_id: req.project_id.to_string(),
        session_id: req.session_id.to_string(),
        project_id: req.project_id.to_string(),
        run_id: Some(req.run_id.to_string()),
        board_task_id: Some(req.board_task_id.to_string()),
        plan_task_id: Some(req.plan_task_id.clone()),
        worker_profile: Some(req.worker_profile.clone()),
        image: opts.image.clone(),
        command: Some(opts.command.clone()),
        args: None,
        env,
        resources: Some(SandboxResources {
            cpu: Some(DEFAULT_CPU.into()),
            memory: Some(DEFAULT_MEMORY.into()),
            ephemeral_storage: Some(DEFAULT_EPHEMERAL.into()),
        }),
        network: Some(network_for_worker_url(&opts.worker_api_url)),
        ttl_seconds: Some(ttl),
    })
}

pub fn cr_labels(req: &SpawnRequest, cr_name: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("app.kubernetes.io/part-of".into(), "agent-fleet".into());
    m.insert("app.kubernetes.io/component".into(), "sandbox".into());
    m.insert(LABEL_SANDBOX.into(), cr_name.into());
    m.insert(LABEL_SESSION.into(), req.session_id.to_string());
    m.insert(LABEL_WORKSPACE.into(), req.project_id.to_string());
    m.insert(LABEL_PROJECT.into(), req.project_id.to_string());
    m
}

pub fn phase_to_state(status: Option<&FleetSandboxStatus>) -> SandboxState {
    let Some(st) = status else {
        return SandboxState::Running;
    };
    let phase = st.phase.as_deref().unwrap_or("");
    match phase {
        "" | "Creating" | "Pending" | "Running" | "Ready" => SandboxState::Running,
        "Succeeded" => SandboxState::Exited { code: Some(0) },
        "Failed" => SandboxState::Exited {
            code: Some(parse_exit_code(st.message.as_deref()).unwrap_or(1)),
        },
        _ => SandboxState::Unknown,
    }
}

pub fn parse_exit_code(message: Option<&str>) -> Option<i32> {
    let msg = message?;
    let rest = msg.strip_prefix("exit ")?;
    let num = rest.split_whitespace().next()?;
    num.parse().ok()
}

fn handle_for(session_id: Uuid) -> RuntimeHandle {
    RuntimeHandle {
        backend: "k8s".into(),
        id: cr_name_for_session(session_id),
    }
}

#[async_trait]
impl SandboxRuntime for K8sRuntime {
    fn backend_name(&self) -> &'static str {
        "k8s"
    }

    async fn spawn(&self, req: SpawnRequest) -> Result<RuntimeHandle, SandboxError> {
        let spec = spec_from_request(&req, &self.opts)?;
        let name = cr_name_for_session(req.session_id);
        let labels = cr_labels(&req, &name);
        let mut sb = FleetSandbox::new(&name, spec);
        sb.meta_mut().namespace = Some(self.opts.namespace.clone());
        sb.meta_mut().labels = Some(labels);

        let api = self.api().await?;
        api.create(&PostParams::default(), &sb)
            .await
            .map_err(|e| SandboxError::Spawn(format!("create FleetSandbox {name}: {e}")))?;
        tracing::info!(
            session_id = %req.session_id,
            sandbox = %name,
            namespace = %self.opts.namespace,
            "k8s sandbox spawned (FleetSandbox CR)"
        );
        Ok(handle_for(req.session_id))
    }

    async fn signal(&self, handle: &RuntimeHandle, signal: Signal) -> Result<(), SandboxError> {
        let grace = match signal {
            Signal::Term => self.opts.kill_grace.as_secs() as u32,
            Signal::Kill => 0,
        };
        delete_cr(&self.api().await?, &handle.id, Some(grace)).await
    }

    async fn destroy(&self, handle: &RuntimeHandle) -> Result<(), SandboxError> {
        delete_cr(&self.api().await?, &handle.id, Some(0)).await
    }

    async fn state(&self, handle: &RuntimeHandle) -> Result<SandboxState, SandboxError> {
        let api = self.api().await?;
        match api.get_opt(&handle.id).await {
            Ok(None) => Ok(SandboxState::Exited { code: None }),
            Ok(Some(sb)) => Ok(phase_to_state(sb.status.as_ref())),
            Err(e) => Err(SandboxError::Other(anyhow::anyhow!("get FleetSandbox: {e}"))),
        }
    }

    async fn list(&self, project_id: Uuid) -> Result<Vec<RuntimeHandle>, SandboxError> {
        let api = self.api().await?;
        let lp = ListParams::default().labels(&format!("{LABEL_PROJECT}={project_id}"));
        let list = api
            .list(&lp)
            .await
            .map_err(|e| SandboxError::Other(anyhow::anyhow!("list FleetSandbox: {e}")))?;
        Ok(list
            .iter()
            .map(|sb| RuntimeHandle {
                backend: "k8s".into(),
                id: sb.name_any(),
            })
            .collect())
    }
}

async fn delete_cr(
    api: &Api<FleetSandbox>,
    name: &str,
    grace_period_seconds: Option<u32>,
) -> Result<(), SandboxError> {
    let dp = DeleteParams {
        grace_period_seconds,
        ..DeleteParams::default()
    };
    match api.delete(name, &dp).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(ae)) if ae.code == 404 => Ok(()),
        Err(e) => Err(SandboxError::Destroy(format!("delete FleetSandbox {name}: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::SpawnRequest;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn sample_req() -> SpawnRequest {
        let mut env = HashMap::new();
        env.insert("FLEET_WORKER_TOKEN".into(), "tok".into());
        env.insert("FLEET_ORCHESTRATOR_URL".into(), "http://127.0.0.1:8080".into());
        env.insert("SECRET_TOKEN".into(), "nope".into());
        SpawnRequest {
            session_id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            project_id: Uuid::parse_str("019fe6e2-d4ad-7a12-9f97-19dc92223a5e").unwrap(),
            run_id: Uuid::parse_str("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee").unwrap(),
            board_task_id: Uuid::parse_str("bbbbbbbb-cccc-4ddd-8eee-ffffffffffff").unwrap(),
            plan_task_id: "t1".into(),
            worker_profile: "coding".into(),
            worker_token: "tok".into(),
            work_dir: PathBuf::from("/tmp/fleet-k8s-test"),
            env,
            timeout: Duration::from_secs(600),
            task_title: Some("Root task".into()),
            task_summary: Some("Write a plan".into()),
            task_spec: Some("Write work/plan.md".into()),
            persona_name: None,
            persona_md: None,
            agent_backend: Some("grok".into()),
            agent_model: Some("grok-4.5".into()),
            io: crate::sandbox::io::IntegrationHandle::none(),
        }
    }

    #[test]
    fn cr_name_is_session_dns_label() {
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        assert_eq!(cr_name_for_session(id), "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn spec_maps_identity_env_ttl_and_strips_non_fleet() {
        let req = sample_req();
        let spec = spec_from_request(&req, &K8sOpts::default()).unwrap();
        assert_eq!(spec.session_id, req.session_id.to_string());
        assert_eq!(spec.project_id, req.project_id.to_string());
        assert_eq!(spec.plan_task_id.as_deref(), Some("t1"));
        assert_eq!(spec.worker_profile.as_deref(), Some("coding"));
        assert_eq!(spec.image, DEFAULT_IMAGE);
        assert_eq!(spec.command.as_ref().unwrap(), &vec!["fleet-worker".to_string()]);
        assert_eq!(spec.ttl_seconds, Some(600));
        assert_eq!(spec.network.as_ref().unwrap().egress_mode, "none");
        assert!(spec.network.as_ref().unwrap().allowlist.is_empty());
        assert_eq!(spec.env.get("FLEET_WORKER_TOKEN").unwrap(), "tok");
        assert_eq!(spec.env.get("FLEET_SPEC_MD").unwrap(), "Write work/plan.md");
        assert_eq!(
            spec.env.get("FLEET_ORCHESTRATOR_URL").unwrap(),
            DEFAULT_WORKER_API_URL
        );
        assert_eq!(spec.env.get("FLEET_WORK_DIR").unwrap(), POD_WORK_DIR);
        assert!(!spec.env.contains_key("SECRET_TOKEN"));
        let labels = cr_labels(&req, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(
            labels.get(LABEL_PROJECT).unwrap(),
            &req.project_id.to_string()
        );
    }

    #[test]
    fn spec_dns_hybrid_callback_is_not_allowlisted() {
        let req = sample_req();
        let mut opts = K8sOpts::default();
        opts.worker_api_url = "http://host.k3d.internal:8080".into();
        let spec = spec_from_request(&req, &opts).unwrap();
        let net = spec.network.as_ref().unwrap();
        assert_eq!(net.egress_mode, "none");
        assert!(net.allowlist.is_empty());
    }

    #[test]
    fn spec_allowlists_host_ip_callback() {
        let req = sample_req();
        let mut opts = K8sOpts::default();
        opts.worker_api_url = "http://172.18.0.1:8081".into();
        let spec = spec_from_request(&req, &opts).unwrap();
        let net = spec.network.as_ref().unwrap();
        assert_eq!(net.egress_mode, "allowlist");
        assert_eq!(net.allowlist, vec!["172.18.0.1/32".to_string()]);
    }

    #[test]
    fn spec_rejects_loopback_callback() {
        let req = sample_req();
        let mut opts = K8sOpts::default();
        opts.worker_api_url = "http://127.0.0.1:8080".into();
        let err = spec_from_request(&req, &opts).unwrap_err().to_string();
        assert!(err.contains("loopback"), "{err}");
        assert!(err.contains("IPv4") || err.contains("literal"), "{err}");
        assert!(!err.contains("host.k3d.internal"), "{err}");
    }

    #[test]
    fn spec_rejects_oversized_md() {
        let mut req = sample_req();
        req.task_spec = Some("x".repeat(MAX_SPEC_MD_BYTES + 1));
        let err = spec_from_request(&req, &K8sOpts::default())
            .unwrap_err()
            .to_string();
        assert!(err.contains("bytes"), "{err}");
    }

    #[test]
    fn new_rejects_localhost_url() {
        let mut opts = K8sOpts::default();
        opts.worker_api_url = "http://localhost:8080".into();
        assert!(K8sRuntime::new(opts).is_err());
    }

    #[test]
    fn phase_mapping() {
        assert_eq!(
            phase_to_state(None),
            SandboxState::Running
        );
        let running = FleetSandboxStatus {
            phase: Some("Creating".into()),
            ..Default::default()
        };
        assert_eq!(phase_to_state(Some(&running)), SandboxState::Running);
        let ok = FleetSandboxStatus {
            phase: Some("Succeeded".into()),
            ..Default::default()
        };
        assert_eq!(
            phase_to_state(Some(&ok)),
            SandboxState::Exited { code: Some(0) }
        );
        let fail = FleetSandboxStatus {
            phase: Some("Failed".into()),
            message: Some("exit 137 OOMKilled".into()),
            ..Default::default()
        };
        assert_eq!(
            phase_to_state(Some(&fail)),
            SandboxState::Exited { code: Some(137) }
        );
    }

    #[test]
    fn parse_exit_from_operator_message() {
        assert_eq!(parse_exit_code(Some("exit 12 Error")), Some(12));
        assert_eq!(parse_exit_code(Some("ImagePullBackOff")), None);
    }
}
