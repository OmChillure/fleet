//! KubePod executor: create / wait / delete `FleetSandbox` CRs.
//!
//! Copied spec builder from `fleet-orchestrator` k8s.rs. Placement is k8s;
//! isolation is still container.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use fleet_operator::crd::{
    FleetSandbox, FleetSandboxSpec, FleetSandboxStatus, LABEL_PROJECT, LABEL_SANDBOX,
    LABEL_SESSION, LABEL_WORKSPACE, SandboxNetwork, SandboxResources, dns_label,
};
use kube::api::{DeleteParams, PostParams};
use kube::{Api, Client, Resource};
use tracing::{info, warn};
use uuid::Uuid;

use crate::client::SpawnPayload;
use crate::config::LocalConfig;
use crate::spawn::{LiveChild, host_child_bin, live_children};

/// etcd / CR env budget (same as orchestrator).
pub const MAX_SPEC_MD_BYTES: usize = 200 * 1024;

pub const DEFAULT_NAMESPACE: &str = "agent-fleet";
const DEFAULT_CPU: &str = "250m";
const DEFAULT_MEMORY: &str = "256Mi";
const DEFAULT_EPHEMERAL: &str = "1Gi";
const POD_WORK_DIR: &str = "/data/work";

#[derive(Debug, Clone)]
pub struct KubePodOpts {
    pub namespace: String,
    pub image: String,
    pub command: Vec<String>,
    pub worker_api_url: String,
    pub cpu: String,
    pub memory: String,
    pub ephemeral: String,
    pub egress_mode: String,
    pub extra_allowlist: Vec<String>,
}

impl KubePodOpts {
    pub fn from_cfg(
        cfg: &LocalConfig,
        payload: &SpawnPayload,
        env: &HashMap<String, String>,
    ) -> Self {
        let bin = Path::new(host_child_bin(cfg, env))
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("fleet-driver")
            .to_string();
        Self {
            namespace: if cfg.k8s_namespace.trim().is_empty() {
                namespace_from_env()
            } else {
                cfg.k8s_namespace.trim().to_string()
            },
            image: cfg.docker_image.clone(),
            command: vec![bin],
            worker_api_url: payload.orchestrator_url.clone(),
            cpu: nonempty(&cfg.k8s_cpu, DEFAULT_CPU),
            memory: nonempty(&cfg.k8s_memory, DEFAULT_MEMORY),
            ephemeral: nonempty(&cfg.k8s_ephemeral, DEFAULT_EPHEMERAL),
            egress_mode: job_egress_mode(payload, cfg),
            extra_allowlist: split_allowlist(&payload.egress_allowlist),
        }
    }
}

fn split_allowlist(raw: &str) -> Vec<String> {
    raw.split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn job_egress_mode(payload: &SpawnPayload, cfg: &LocalConfig) -> String {
    let job = payload.egress_mode.trim();
    if job.is_empty() || job.eq_ignore_ascii_case("inherit") {
        nonempty(&cfg.egress_mode, "none")
    } else {
        job.to_ascii_lowercase()
    }
}

fn nonempty(s: &str, fallback: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        fallback.to_string()
    } else {
        t.to_string()
    }
}

fn network_for_opts(opts: &KubePodOpts) -> SandboxNetwork {
    let mode = opts.egress_mode.trim().to_ascii_lowercase();
    if mode == "open" {
        return SandboxNetwork {
            egress_mode: "open".into(),
            allowlist: vec![],
        };
    }
    // A host IP callback needs a CIDR or NetworkPolicy blocks heartbeats,
    // even when the box default is egress none.
    if mode == "allowlist" || worker_url_is_ip_literal(&opts.worker_api_url) {
        let mut net = network_for_worker_url(&opts.worker_api_url);
        if mode == "allowlist" {
            net.egress_mode = "allowlist".into();
            for cidr in &opts.extra_allowlist {
                if !net.allowlist.iter().any(|c| c == cidr) {
                    net.allowlist.push(cidr.clone());
                }
            }
        }
        return net;
    }
    SandboxNetwork {
        egress_mode: if mode == "dns_only" {
            "dns_only".into()
        } else {
            "none".into()
        },
        allowlist: vec![],
    }
}

fn worker_url_is_ip_literal(url: &str) -> bool {
    url_host(url)
        .map(|host| is_ipv4_literal(&host) || is_ipv6_literal(&host))
        .unwrap_or(false)
}

fn namespace_from_env() -> String {
    std::env::var("K8S_NAMESPACE")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_NAMESPACE.to_string())
}

/// DNS-1123 CR name from a session id (pod will be `fsb-<name>`).
pub fn cr_name_for_session(session_id: Uuid) -> String {
    dns_label(&session_id.to_string())
}

pub fn loopback_worker_url_error(url: &str) -> Option<String> {
    let host = url_host(url)?;
    if is_loopback_host(&host) {
        Some(format!(
            "worker API URL {url} is loopback; pods cannot reach the API host. \
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
    if host.is_empty() { None } else { Some(host) }
}

fn is_loopback_host(host: &str) -> bool {
    agent_fleet_contracts::host_is_loopback(host)
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

pub fn spec_from_payload(
    payload: &SpawnPayload,
    env: &HashMap<String, String>,
    opts: &KubePodOpts,
) -> Result<FleetSandboxSpec> {
    if opts.worker_api_url.trim().is_empty() {
        bail!("kube_pod requires orchestrator_url");
    }
    if let Some(msg) = loopback_worker_url_error(&opts.worker_api_url) {
        bail!("{msg}");
    }
    if spec_md_too_large(&payload.task_spec) {
        bail!(
            "task spec is {} bytes; k8s env limit is {MAX_SPEC_MD_BYTES}",
            payload.task_spec.len()
        );
    }

    let mut cr_env = BTreeMap::new();
    for (k, v) in env {
        if k.starts_with("FLEET_")
            || matches!(
                k.as_str(),
                "XAI_API_KEY" | "ANTHROPIC_API_KEY" | "ANTHROPIC_AUTH_TOKEN"
            )
        {
            let u = k.to_ascii_uppercase();
            if u.contains("AUTH_JSON") || u.contains("SESSION") {
                continue;
            }
            cr_env.insert(k.clone(), v.clone());
        }
    }
    cr_env.insert("FLEET_ORCHESTRATOR_URL".into(), opts.worker_api_url.clone());
    cr_env.insert("FLEET_API_BASE".into(), opts.worker_api_url.clone());
    cr_env.insert("FLEET_WORK_DIR".into(), POD_WORK_DIR.into());
    if !payload.task_spec.is_empty() {
        cr_env.insert("FLEET_SPEC_MD".into(), payload.task_spec.clone());
    }
    if !payload.task_title.is_empty() {
        cr_env.insert("FLEET_TASK_TITLE".into(), payload.task_title.clone());
    }

    let ttl = payload.timeout_s.max(1) as i64;
    Ok(FleetSandboxSpec {
        workspace_id: payload.project_id.to_string(),
        session_id: payload.session_id.to_string(),
        project_id: payload.project_id.to_string(),
        run_id: Some(payload.run_id.to_string()),
        board_task_id: Some(payload.board_task_id.to_string()),
        plan_task_id: Some(payload.plan_task_id.clone()),
        worker_profile: Some(payload.worker_profile.clone()),
        image: opts.image.clone(),
        command: Some(opts.command.clone()),
        args: None,
        env: cr_env,
        resources: Some(SandboxResources {
            cpu: Some(opts.cpu.clone()),
            memory: Some(opts.memory.clone()),
            ephemeral_storage: Some(opts.ephemeral.clone()),
        }),
        network: Some(network_for_opts(opts)),
        ttl_seconds: Some(ttl),
    })
}

pub fn cr_labels(payload: &SpawnPayload, cr_name: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("app.kubernetes.io/part-of".into(), "agent-fleet".into());
    m.insert("app.kubernetes.io/component".into(), "sandbox".into());
    m.insert(LABEL_SANDBOX.into(), cr_name.into());
    m.insert(LABEL_SESSION.into(), payload.session_id.to_string());
    m.insert(LABEL_WORKSPACE.into(), payload.project_id.to_string());
    m.insert(LABEL_PROJECT.into(), payload.project_id.to_string());
    m
}

pub fn phase_to_exit(status: Option<&FleetSandboxStatus>) -> Option<Result<()>> {
    let st = status?;
    match st.phase.as_deref().unwrap_or("") {
        "Succeeded" => Some(Ok(())),
        "Failed" => Some(Err(anyhow::anyhow!(
            "FleetSandbox failed: {}",
            st.message.as_deref().unwrap_or("unknown")
        ))),
        _ => None,
    }
}

pub async fn run_kube_pod(
    cfg: &LocalConfig,
    payload: &SpawnPayload,
    session_dir: &Path,
) -> Result<()> {
    let env = crate::spawn::child_env_for_isolated_guest(
        payload,
        &cfg.extra_env,
        Path::new(POD_WORK_DIR),
    );
    let opts = KubePodOpts::from_cfg(cfg, payload, &env);
    if opts.image.trim().is_empty() {
        bail!("kube_pod image is empty");
    }
    let spec = spec_from_payload(payload, &env, &opts)?;
    let name = cr_name_for_session(payload.session_id);
    let labels = cr_labels(payload, &name);

    let client = Client::try_default().await.context(
        "k8s client (in-cluster config or KUBECONFIG); container isolation will not fall back to host",
    )?;
    let api: Api<FleetSandbox> = Api::namespaced(client, &opts.namespace);

    let mut sb = FleetSandbox::new(&name, spec);
    sb.meta_mut().namespace = Some(opts.namespace.clone());
    sb.meta_mut().labels = Some(labels);

    api.create(&PostParams::default(), &sb)
        .await
        .with_context(|| format!("create FleetSandbox {name}"))?;
    info!(
        session_id = %payload.session_id,
        sandbox = %name,
        namespace = %opts.namespace,
        "kube_pod spawned (FleetSandbox CR)"
    );

    live_children()
        .insert(
            payload.session_id,
            LiveChild::Kube {
                name: name.clone(),
                namespace: opts.namespace.clone(),
            },
        )
        .await;

    let result = wait_cr(&api, &name, payload.timeout_s).await;
    if result.is_ok() {
        pull_guest_proof(&opts.namespace, &name, session_dir).await;
    }
    live_children().remove(payload.session_id).await;
    let _ = delete_sandbox_api(&api, &name).await;
    result
}

async fn wait_cr(api: &Api<FleetSandbox>, name: &str, timeout_s: u64) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_s.max(1));
    loop {
        match api.get_opt(name).await {
            Ok(None) => bail!("FleetSandbox {name} disappeared"),
            Ok(Some(sb)) => {
                if let Some(done) = phase_to_exit(sb.status.as_ref()) {
                    return done;
                }
            }
            Err(e) => warn!(error = %e, name = %name, "get FleetSandbox"),
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("FleetSandbox {name} timed out after {timeout_s}s");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

pub async fn pull_guest_proof(namespace: &str, cr_name: &str, dest: &Path) {
    if dest.join("result.md").is_file() {
        return;
    }
    if !command_exists("kubectl") {
        return;
    }
    let pod = fleet_operator::crd::pod_name_for(cr_name);
    for rel in [
        "result.md",
        "artifacts/transcript.md",
        "artifacts/result.json",
    ] {
        let mut kubectl = tokio::process::Command::new("kubectl");
        crate::platform::hide_console_tokio(&mut kubectl);
        let out = kubectl
            .args([
                "-n",
                namespace,
                "exec",
                &pod,
                "-c",
                "worker",
                "--",
                "cat",
                &format!("{POD_WORK_DIR}/{rel}"),
            ])
            .output()
            .await;
        let Ok(out) = out else {
            continue;
        };
        if !out.status.success() || out.stdout.is_empty() {
            continue;
        }
        let path = dest.join(rel);
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let _ = tokio::fs::write(&path, &out.stdout).await;
    }
    if !dest.join("result.md").is_file() {
        if let Some(md) = crate::spawn::collect_guest_proof(dest) {
            let _ = tokio::fs::write(dest.join("result.md"), md).await;
        }
    }
}

fn command_exists(bin: &str) -> bool {
    crate::platform::command_exists(bin)
}

pub async fn delete_sandbox(name: &str, namespace: &str) -> Result<()> {
    let client = Client::try_default()
        .await
        .context("k8s client (in-cluster config or KUBECONFIG)")?;
    let api: Api<FleetSandbox> = Api::namespaced(client, namespace);
    delete_sandbox_api(&api, name).await
}

async fn delete_sandbox_api(api: &Api<FleetSandbox>, name: &str) -> Result<()> {
    let dp = DeleteParams {
        grace_period_seconds: Some(15),
        ..DeleteParams::default()
    };
    match api.delete(name, &dp).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(ae)) if ae.code == 404 => Ok(()),
        Err(e) => Err(anyhow::anyhow!("delete FleetSandbox {name}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::JobRuntime;
    use std::collections::HashMap;

    fn sample_payload() -> SpawnPayload {
        SpawnPayload {
            kind: String::new(),
            session_id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            project_id: Uuid::parse_str("019fe6e2-d4ad-7a12-9f97-19dc92223a5e").unwrap(),
            run_id: Uuid::parse_str("aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee").unwrap(),
            board_task_id: Uuid::parse_str("bbbbbbbb-cccc-4ddd-8eee-ffffffffffff").unwrap(),
            plan_task_id: "t1".into(),
            worker_profile: "coding".into(),
            worker_token: "tok".into(),
            orchestrator_url: "http://api.agent-fleet.svc.cluster.local:8080".into(),
            runtime: JobRuntime::Container,
            run_mode: Some("in_container".into()),
            egress_mode: String::new(),
            egress_allowlist: String::new(),
            isolation: Some("container".into()),
            task_title: "Root task".into(),
            task_spec: "Write work/plan.md".into(),
            timeout_s: 600,
            repo_url: None,
            git_ref: None,
            persona_md: None,
            env: HashMap::new(),
            privileged: false,
            workspace_files: HashMap::new(),
        }
    }

    fn sample_opts() -> KubePodOpts {
        KubePodOpts {
            namespace: DEFAULT_NAMESPACE.into(),
            image: "agent-fleet-api:local".into(),
            command: vec!["fleet-driver".into()],
            worker_api_url: "http://api.agent-fleet.svc.cluster.local:8080".into(),
            cpu: DEFAULT_CPU.into(),
            memory: DEFAULT_MEMORY.into(),
            ephemeral: DEFAULT_EPHEMERAL.into(),
            egress_mode: "none".into(),
            extra_allowlist: vec![],
        }
    }

    #[test]
    fn cr_name_is_session_dns_label() {
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        assert_eq!(
            cr_name_for_session(id),
            "550e8400-e29b-41d4-a716-446655440000"
        );
    }

    #[test]
    fn from_cfg_guest_is_fleet_driver_without_harness() {
        let cfg = LocalConfig::defaults();
        let payload = sample_payload();
        let env = HashMap::new();
        let opts = KubePodOpts::from_cfg(&cfg, &payload, &env);
        assert_eq!(opts.command, vec!["fleet-driver".to_string()]);
    }

    #[test]
    fn from_cfg_command_is_basename_of_absolute_driver_bin() {
        let mut cfg = LocalConfig::defaults();
        cfg.fleet_driver_bin = "/opt/fleet/fleet-driver".into();
        let opts = KubePodOpts::from_cfg(&cfg, &sample_payload(), &HashMap::new());
        assert_eq!(opts.command, vec!["fleet-driver".to_string()]);
    }

    #[test]
    fn from_cfg_command_ignores_fleet_harness_env() {
        let cfg = LocalConfig::defaults();
        let mut env = HashMap::new();
        env.insert("FLEET_HARNESS".into(), "grok".into());
        let opts = KubePodOpts::from_cfg(&cfg, &sample_payload(), &env);
        assert_eq!(opts.command, vec!["fleet-driver".to_string()]);
    }

    #[test]
    fn spec_strips_non_fleet_keeps_fleet_env() {
        let payload = sample_payload();
        let mut env = HashMap::new();
        env.insert("FLEET_WORKER_TOKEN".into(), "tok".into());
        env.insert("SECRET_TOKEN".into(), "nope".into());
        let spec = spec_from_payload(&payload, &env, &sample_opts()).unwrap();
        assert_eq!(spec.env.get("FLEET_WORKER_TOKEN").unwrap(), "tok");
        assert_eq!(spec.env.get("FLEET_SPEC_MD").unwrap(), "Write work/plan.md");
        assert_eq!(spec.env.get("FLEET_WORK_DIR").unwrap(), POD_WORK_DIR);
        assert!(!spec.env.contains_key("SECRET_TOKEN"));
    }

    #[test]
    fn spec_maps_identity_and_ttl() {
        let payload = sample_payload();
        let spec = spec_from_payload(&payload, &HashMap::new(), &sample_opts()).unwrap();
        assert_eq!(spec.session_id, payload.session_id.to_string());
        assert_eq!(spec.project_id, payload.project_id.to_string());
        assert_eq!(spec.plan_task_id.as_deref(), Some("t1"));
        assert_eq!(spec.worker_profile.as_deref(), Some("coding"));
        assert_eq!(spec.ttl_seconds, Some(600));
    }

    #[test]
    fn cr_labels_sets_project() {
        let payload = sample_payload();
        let labels = cr_labels(&payload, "550e8400-e29b-41d4-a716-446655440000");
        assert_eq!(
            labels.get(LABEL_PROJECT).unwrap(),
            &payload.project_id.to_string()
        );
    }

    #[test]
    fn spec_overwrites_orchestrator_url_and_api_base_from_opts() {
        let mut env = HashMap::new();
        env.insert("FLEET_ORCHESTRATOR_URL".into(), "http://stale:1".into());
        env.insert("FLEET_API_BASE".into(), "http://stale:1".into());
        let opts = sample_opts();
        let spec = spec_from_payload(&sample_payload(), &env, &opts).unwrap();
        assert_eq!(
            spec.env.get("FLEET_ORCHESTRATOR_URL").unwrap(),
            &opts.worker_api_url
        );
        assert_eq!(
            spec.env.get("FLEET_API_BASE").unwrap(),
            &opts.worker_api_url
        );
    }

    #[test]
    fn spec_ttl_clamps_zero_timeout_to_one() {
        let mut payload = sample_payload();
        payload.timeout_s = 0;
        let spec = spec_from_payload(&payload, &HashMap::new(), &sample_opts()).unwrap();
        assert_eq!(spec.ttl_seconds, Some(1));
    }

    #[test]
    fn cr_labels_sets_session_workspace_and_sandbox() {
        let payload = sample_payload();
        let cr_name = "550e8400-e29b-41d4-a716-446655440000";
        let labels = cr_labels(&payload, cr_name);
        assert_eq!(
            labels.get(LABEL_SESSION).unwrap(),
            &payload.session_id.to_string()
        );
        assert_eq!(
            labels.get(LABEL_WORKSPACE).unwrap(),
            &payload.project_id.to_string()
        );
        assert_eq!(labels.get(LABEL_SANDBOX).unwrap(), cr_name);
    }

    #[test]
    fn spec_allowlists_host_ip_callback() {
        let payload = sample_payload();
        let mut opts = sample_opts();
        opts.worker_api_url = "http://172.18.0.1:8081".into();
        let spec = spec_from_payload(&payload, &HashMap::new(), &opts).unwrap();
        let net = spec.network.as_ref().unwrap();
        assert_eq!(net.egress_mode, "allowlist");
        assert_eq!(net.allowlist, vec!["172.18.0.1/32".to_string()]);
    }

    #[test]
    fn spec_rejects_loopback_callback() {
        let payload = sample_payload();
        let mut opts = sample_opts();
        opts.worker_api_url = "http://127.0.0.1:8080".into();
        let err = spec_from_payload(&payload, &HashMap::new(), &opts)
            .unwrap_err()
            .to_string();
        assert!(err.contains("loopback"), "{err}");
    }

    #[test]
    fn spec_rejects_oversized_md() {
        let mut payload = sample_payload();
        payload.task_spec = "x".repeat(MAX_SPEC_MD_BYTES + 1);
        match spec_from_payload(&payload, &HashMap::new(), &sample_opts()) {
            Ok(spec) => panic!(
                "oversized task_spec became FLEET_SPEC_MD (len={})",
                spec.env.get("FLEET_SPEC_MD").map(String::len).unwrap_or(0)
            ),
            Err(err) => {
                let err = err.to_string();
                assert!(err.contains(&MAX_SPEC_MD_BYTES.to_string()), "{err}");
            }
        }
    }

    #[test]
    fn spec_accepts_md_at_max_bytes() {
        let mut payload = sample_payload();
        payload.task_spec = "x".repeat(MAX_SPEC_MD_BYTES);
        let spec = spec_from_payload(&payload, &HashMap::new(), &sample_opts()).unwrap();
        assert_eq!(spec.env.get("FLEET_SPEC_MD"), Some(&payload.task_spec));
    }

    #[test]
    fn spec_omits_fleet_spec_md_when_task_spec_empty() {
        let mut payload = sample_payload();
        payload.task_spec.clear();
        let spec = spec_from_payload(&payload, &HashMap::new(), &sample_opts()).unwrap();
        assert!(!spec.env.contains_key("FLEET_SPEC_MD"));
    }

    #[test]
    fn spec_requires_orchestrator_url() {
        let payload = sample_payload();
        let mut opts = sample_opts();
        opts.worker_api_url.clear();
        let err = spec_from_payload(&payload, &HashMap::new(), &opts)
            .unwrap_err()
            .to_string();
        assert!(err.contains("orchestrator_url"), "{err}");
    }

    #[test]
    fn kube_pod_env_drops_host_absolute_bin_overrides() {
        let mut extra = HashMap::new();
        extra.insert("FLEET_MOCK_ACP_BIN".into(), "/host/mock-acp-agent".into());
        let mut payload = sample_payload();
        payload
            .env
            .insert("FLEET_WORK_CMD".into(), "echo host-only".into());
        let env =
            crate::spawn::child_env_for_isolated_guest(&payload, &extra, Path::new(POD_WORK_DIR));
        let spec = spec_from_payload(&payload, &env, &sample_opts()).unwrap();
        assert!(
            !spec.env.contains_key("FLEET_MOCK_ACP_BIN"),
            "host path must not ride into kube: {:?}",
            spec.env
        );
        assert!(
            !spec.env.contains_key("FLEET_WORK_CMD"),
            "FLEET_WORK_CMD is host-only: {:?}",
            spec.env
        );
    }
}
