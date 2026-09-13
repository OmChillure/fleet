//! Reconcile FleetSandbox → Pod + NetworkPolicy.

use crate::crd::{
    FINALIZER, FleetSandbox, FleetSandboxStatus, LABEL_PROJECT, LABEL_SANDBOX, LABEL_SESSION,
    LABEL_WORKSPACE, pod_name_for,
};
use chrono::Utc;
use k8s_openapi::api::core::v1::{
    Capabilities, Container, EmptyDirVolumeSource, EnvVar, Pod, PodSpec, PodStatus,
    ResourceRequirements, SecurityContext, Volume, VolumeMount,
};
use k8s_openapi::api::networking::v1::{
    IPBlock, NetworkPolicy, NetworkPolicyEgressRule, NetworkPolicyPeer, NetworkPolicyPort,
    NetworkPolicySpec,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::api::{DeleteParams, Patch, PatchParams, PostParams};
use kube::runtime::controller::Action;
use kube::runtime::finalizer::{Event as FinalizerEvent, finalizer};
use kube::{Api, Client, Resource, ResourceExt};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

#[derive(Clone, Debug)]
pub struct OperatorConfig {
    pub worker_image_default: String,
    pub api_service_url: String,
    pub api_namespace: String,
    pub api_pod_label_name: String,
}

impl OperatorConfig {
    pub fn from_env() -> Self {
        Self {
            worker_image_default: std::env::var("WORKER_IMAGE")
                .unwrap_or_else(|_| "agent-fleet-api:local".into()),
            api_service_url: std::env::var("API_SERVICE_URL")
                .unwrap_or_else(|_| "http://api.agent-fleet.svc.cluster.local:8080".into()),
            api_namespace: std::env::var("API_NAMESPACE").unwrap_or_else(|_| "agent-fleet".into()),
            api_pod_label_name: std::env::var("API_POD_LABEL")
                .unwrap_or_else(|_| "fleet-api".into()),
        }
    }
}

pub struct Context {
    pub client: Client,
    pub cfg: OperatorConfig,
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("kube: {0}")]
    Kube(#[from] kube::Error),
    #[error("finalizer: {0}")]
    Finalizer(String),
    #[error("{0}")]
    Other(String),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub async fn reconcile(sb: Arc<FleetSandbox>, ctx: Arc<Context>) -> Result<Action> {
    let ns = sb
        .namespace()
        .ok_or_else(|| Error::Other("missing namespace".into()))?;
    let api: Api<FleetSandbox> = Api::namespaced(ctx.client.clone(), &ns);

    finalizer(&api, FINALIZER, sb, |event| async {
        match event {
            FinalizerEvent::Apply(s) => apply(&s, &ctx).await,
            FinalizerEvent::Cleanup(s) => cleanup(&s, &ctx).await,
        }
    })
    .await
    .map_err(|e| Error::Finalizer(e.to_string()))
}

pub fn error_policy(_sb: Arc<FleetSandbox>, err: &Error, _ctx: Arc<Context>) -> Action {
    warn!(error = %err, "reconcile failed; requeue");
    Action::requeue(Duration::from_secs(15))
}

async fn apply(sb: &FleetSandbox, ctx: &Context) -> Result<Action> {
    let ns = sb.namespace().unwrap();
    let name = sb.name_any();
    let pod_name = pod_name_for(&name);
    let pods: Api<Pod> = Api::namespaced(ctx.client.clone(), &ns);
    let nps: Api<NetworkPolicy> = Api::namespaced(ctx.client.clone(), &ns);
    let sbs: Api<FleetSandbox> = Api::namespaced(ctx.client.clone(), &ns);

    match apply_decision(sb) {
        ApplyDecision::DeleteCr => {
            info!(sandbox = %name, "ttl exceeded; deleting FleetSandbox");
            sbs.delete(&name, &DeleteParams::default()).await?;
            return Ok(Action::await_change());
        }
        ApplyDecision::Continue => {}
    }

    let desired_pod = build_pod(sb, &pod_name, &ctx.cfg);
    let desired_np = build_network_policy(sb, &pod_name, &ctx.cfg);

    match pods.get_opt(&pod_name).await? {
        None => {
            info!(sandbox = %name, pod = %pod_name, "creating sandbox pod");
            pods.create(&PostParams::default(), &desired_pod).await?;
            apply_np(&nps, &desired_np).await?;
            patch_status(
                &sbs,
                &name,
                FleetSandboxStatus {
                    phase: Some("Creating".into()),
                    pod_name: Some(pod_name),
                    message: Some("pod created".into()),
                    observed_generation: sb.metadata.generation,
                    ..Default::default()
                },
            )
            .await?;
            return Ok(Action::requeue(Duration::from_secs(5)));
        }
        Some(pod) => {
            apply_np(&nps, &desired_np).await?;
            let (phase, msg, node) = interpret_pod(&pod);
            let started = sb
                .status
                .as_ref()
                .and_then(|s| s.started_at.clone())
                .or_else(|| {
                    if phase == "Running" || phase == "Ready" {
                        Some(Utc::now().to_rfc3339())
                    } else {
                        None
                    }
                });
            patch_status(
                &sbs,
                &name,
                FleetSandboxStatus {
                    phase: Some(phase.clone()),
                    pod_name: Some(pod_name),
                    node_name: node,
                    message: Some(msg),
                    started_at: started,
                    observed_generation: sb.metadata.generation,
                },
            )
            .await?;

            if phase == "Failed" || phase == "Succeeded" {
                return Ok(Action::requeue(Duration::from_secs(30)));
            }
            return Ok(Action::requeue(Duration::from_secs(10)));
        }
    }
}

async fn cleanup(sb: &FleetSandbox, ctx: &Context) -> Result<Action> {
    let ns = sb.namespace().unwrap();
    let name = sb.name_any();
    let pod_name = pod_name_for(&name);
    let pods: Api<Pod> = Api::namespaced(ctx.client.clone(), &ns);
    let nps: Api<NetworkPolicy> = Api::namespaced(ctx.client.clone(), &ns);
    let np_name = network_policy_name(&pod_name);

    if pods.get_opt(&pod_name).await?.is_some() {
        info!(sandbox = %name, pod = %pod_name, "deleting sandbox pod");
        if let Err(e) = pods.delete(&pod_name, &DeleteParams::default()).await {
            if !matches!(&e, kube::Error::Api(ae) if ae.code == 404) {
                warn!(sandbox = %name, pod = %pod_name, error = %e, "pod delete failed");
                return Err(e.into());
            }
        }
    }
    if nps.get_opt(&np_name).await?.is_some() {
        if let Err(e) = nps.delete(&np_name, &DeleteParams::default()).await {
            if !matches!(&e, kube::Error::Api(ae) if ae.code == 404) {
                warn!(sandbox = %name, np = %np_name, error = %e, "networkpolicy delete failed");
                return Err(e.into());
            }
        }
    }
    Ok(Action::await_change())
}

async fn apply_np(api: &Api<NetworkPolicy>, np: &NetworkPolicy) -> Result<()> {
    let name = np.metadata.name.clone().unwrap_or_default();
    api.patch(
        &name,
        &PatchParams::apply("fleet-operator").force(),
        &Patch::Apply(np),
    )
    .await?;
    Ok(())
}

async fn patch_status(
    api: &Api<FleetSandbox>,
    name: &str,
    status: FleetSandboxStatus,
) -> Result<()> {
    let patch = serde_json::json!({ "status": status });
    api.patch_status(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await?;
    Ok(())
}

fn ttl_expired(sb: &FleetSandbox) -> bool {
    let Some(ttl) = sb.spec.ttl_seconds.filter(|t| *t > 0) else {
        return false;
    };
    let created = sb.metadata.creation_timestamp.as_ref().map(|t| t.0);
    let Some(created) = created else {
        return false;
    };
    let age = Utc::now().signed_duration_since(created);
    age.num_seconds() >= ttl
}

/// Docs §13: `ttl_seconds` exceeded → delete the FleetSandbox CR.
/// `Continue` is create-pod / patch-Running — never that after TTL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApplyDecision {
    DeleteCr,
    Continue,
}

/// What `apply()` does for this object. Age `>= ttl` is always `DeleteCr`.
pub(crate) fn apply_decision(sb: &FleetSandbox) -> ApplyDecision {
    if ttl_expired(sb) {
        ApplyDecision::DeleteCr
    } else {
        ApplyDecision::Continue
    }
}

pub fn build_pod(sb: &FleetSandbox, pod_name: &str, cfg: &OperatorConfig) -> Pod {
    let name = sb.name_any();
    let labels = common_labels(sb, &name);
    let image = if sb.spec.image.trim().is_empty() {
        cfg.worker_image_default.clone()
    } else {
        sb.spec.image.clone()
    };

    let mut env: Vec<EnvVar> = vec![
        env_kv("FLEET_WORK_DIR", "/data/work"),
        env_kv("FLEET_ORCHESTRATOR_URL", &cfg.api_service_url),
        env_kv("FLEET_API_BASE", &cfg.api_service_url),
        env_kv("FLEET_SESSION_ID", &sb.spec.session_id),
        env_kv("FLEET_PROJECT_ID", &sb.spec.project_id),
        env_kv(
            "FLEET_WORKER_PROFILE",
            sb.spec.worker_profile.as_deref().unwrap_or("generalist"),
        ),
    ];
    if let Some(v) = &sb.spec.run_id {
        env.push(env_kv("FLEET_RUN_ID", v));
    }
    if let Some(v) = &sb.spec.board_task_id {
        env.push(env_kv("FLEET_BOARD_TASK_ID", v));
        env.push(env_kv("FLEET_TASK_ID", v));
    }
    if let Some(v) = &sb.spec.plan_task_id {
        env.push(env_kv("FLEET_PLAN_TASK_ID", v));
    }
    for (k, v) in &sb.spec.env {
        // Spec env wins over defaults (retain last).
        env.retain(|e| e.name != *k);
        env.push(env_kv(k, v));
    }

    let command = sb
        .spec
        .command
        .clone()
        .unwrap_or_else(|| vec!["fleet-driver".into()]);

    let resources = sb.spec.resources.as_ref().map(|r| {
        let mut limits = BTreeMap::new();
        if let Some(cpu) = &r.cpu {
            limits.insert("cpu".into(), Quantity(cpu.clone()));
        }
        if let Some(mem) = &r.memory {
            limits.insert("memory".into(), Quantity(mem.clone()));
        }
        if let Some(ephem) = &r.ephemeral_storage {
            limits.insert("ephemeral-storage".into(), Quantity(ephem.clone()));
        }
        ResourceRequirements {
            limits: Some(limits.clone()),
            requests: Some(limits),
            ..Default::default()
        }
    });

    let owner = sb.controller_owner_ref(&()).into_iter().collect();

    Pod {
        metadata: ObjectMeta {
            name: Some(pod_name.to_string()),
            namespace: sb.namespace(),
            labels: Some(labels),
            owner_references: Some(owner),
            ..Default::default()
        },
        spec: Some(PodSpec {
            restart_policy: Some("Never".into()),
            automount_service_account_token: Some(false),
            security_context: Some(k8s_openapi::api::core::v1::PodSecurityContext {
                run_as_non_root: Some(true),
                run_as_user: Some(10001),
                fs_group: Some(10001),
                seccomp_profile: Some(k8s_openapi::api::core::v1::SeccompProfile {
                    type_: "RuntimeDefault".into(),
                    localhost_profile: None,
                }),
                ..Default::default()
            }),
            containers: vec![Container {
                name: "worker".into(),
                image: Some(image),
                command: Some(command),
                args: sb.spec.args.clone(),
                env: Some(env),
                working_dir: Some("/data/work".into()),
                resources,
                security_context: Some(SecurityContext {
                    allow_privilege_escalation: Some(false),
                    read_only_root_filesystem: Some(false),
                    run_as_non_root: Some(true),
                    run_as_user: Some(10001),
                    capabilities: Some(Capabilities {
                        drop: Some(vec!["ALL".into()]),
                        add: None,
                    }),
                    ..Default::default()
                }),
                volume_mounts: Some(vec![VolumeMount {
                    name: "work".into(),
                    mount_path: "/data/work".into(),
                    ..Default::default()
                }]),
                ..Default::default()
            }],
            volumes: Some(vec![Volume {
                name: "work".into(),
                empty_dir: Some(EmptyDirVolumeSource {
                    medium: None,
                    size_limit: sb
                        .spec
                        .resources
                        .as_ref()
                        .and_then(|r| r.ephemeral_storage.as_ref().map(|s| Quantity(s.clone()))),
                }),
                ..Default::default()
            }]),
            ..Default::default()
        }),
        status: None,
    }
}

pub fn build_network_policy(
    sb: &FleetSandbox,
    pod_name: &str,
    cfg: &OperatorConfig,
) -> NetworkPolicy {
    let name = sb.name_any();
    let labels = common_labels(sb, &name);
    let owner = sb.controller_owner_ref(&()).into_iter().collect();
    let np_name = network_policy_name(pod_name);

    let dns_peer = NetworkPolicyPeer {
        namespace_selector: Some(LabelSelector {
            match_labels: Some(BTreeMap::from([(
                "kubernetes.io/metadata.name".into(),
                "kube-system".into(),
            )])),
            match_expressions: None,
        }),
        ..Default::default()
    };
    let dns_ports = vec![np_port(53, "UDP"), np_port(53, "TCP")];

    let api_peer = NetworkPolicyPeer {
        namespace_selector: Some(LabelSelector {
            match_labels: Some(BTreeMap::from([(
                "kubernetes.io/metadata.name".into(),
                cfg.api_namespace.clone(),
            )])),
            match_expressions: None,
        }),
        pod_selector: Some(LabelSelector {
            match_labels: Some(BTreeMap::from([(
                "app.kubernetes.io/name".into(),
                cfg.api_pod_label_name.clone(),
            )])),
            match_expressions: None,
        }),
        ..Default::default()
    };

    let mut egress = vec![
        NetworkPolicyEgressRule {
            to: Some(vec![dns_peer]),
            ports: Some(dns_ports),
        },
        NetworkPolicyEgressRule {
            to: Some(vec![api_peer]),
            ports: Some(vec![np_port(8080, "TCP")]),
        },
    ];

    if sb.spec.egress_mode() == "allowlist" {
        if let Some(net) = &sb.spec.network {
            for cidr in &net.allowlist {
                if cidr.contains('/') {
                    egress.push(NetworkPolicyEgressRule {
                        to: Some(vec![NetworkPolicyPeer {
                            ip_block: Some(IPBlock {
                                cidr: cidr.clone(),
                                except: None,
                            }),
                            ..Default::default()
                        }]),
                        ports: None,
                    });
                }
            }
        }
    }

    NetworkPolicy {
        metadata: ObjectMeta {
            name: Some(np_name),
            namespace: sb.namespace(),
            labels: Some(labels.clone()),
            owner_references: Some(owner),
            ..Default::default()
        },
        spec: Some(NetworkPolicySpec {
            pod_selector: LabelSelector {
                match_labels: Some(BTreeMap::from([(LABEL_SANDBOX.to_string(), name)])),
                match_expressions: None,
            },
            policy_types: Some(vec!["Ingress".into(), "Egress".into()]),
            ingress: Some(vec![]),
            egress: Some(egress),
        }),
    }
}

fn network_policy_name(pod_name: &str) -> String {
    let mut n = format!("{pod_name}-net");
    n.truncate(63);
    n.trim_end_matches('-').to_string()
}

fn common_labels(sb: &FleetSandbox, sandbox_name: &str) -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    m.insert("app.kubernetes.io/part-of".into(), "agent-fleet".into());
    m.insert("app.kubernetes.io/component".into(), "sandbox".into());
    m.insert(LABEL_SANDBOX.into(), sandbox_name.into());
    m.insert(LABEL_SESSION.into(), sb.spec.session_id.clone());
    m.insert(LABEL_WORKSPACE.into(), sb.spec.workspace_id.clone());
    m.insert(LABEL_PROJECT.into(), sb.spec.project_id.clone());
    m
}

fn env_kv(k: &str, v: &str) -> EnvVar {
    EnvVar {
        name: k.into(),
        value: Some(v.into()),
        ..Default::default()
    }
}

fn np_port(port: i32, proto: &str) -> NetworkPolicyPort {
    NetworkPolicyPort {
        port: Some(IntOrString::Int(port)),
        protocol: Some(proto.into()),
        end_port: None,
    }
}

fn interpret_pod(pod: &Pod) -> (String, String, Option<String>) {
    let node = pod.spec.as_ref().and_then(|s| s.node_name.clone());
    let Some(st): Option<&PodStatus> = pod.status.as_ref() else {
        return ("Pending".into(), "no status yet".into(), node);
    };
    let phase = st.phase.clone().unwrap_or_else(|| "Unknown".into());
    let msg = st
        .container_statuses
        .as_ref()
        .and_then(|cs| cs.first())
        .and_then(|c| {
            if let Some(w) = &c.state.as_ref().and_then(|s| s.waiting.clone()) {
                return Some(format!(
                    "{}: {}",
                    w.reason.clone().unwrap_or_default(),
                    w.message.clone().unwrap_or_default()
                ));
            }
            if let Some(t) = &c.state.as_ref().and_then(|s| s.terminated.clone()) {
                return Some(format!(
                    "exit {} {}",
                    t.exit_code,
                    t.reason.clone().unwrap_or_default()
                ));
            }
            None
        })
        .or_else(|| st.message.clone())
        .unwrap_or_else(|| phase.clone());

    let mapped = match phase.as_str() {
        "Pending" => "Creating",
        "Running" => "Running",
        "Succeeded" => "Succeeded",
        "Failed" => "Failed",
        other => other,
    };
    (mapped.into(), msg, node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crd::FleetSandboxSpec;
    use crate::crd::SandboxNetwork;

    fn sample_sb() -> FleetSandbox {
        let spec = FleetSandboxSpec {
            workspace_id: "ws-1".into(),
            session_id: "sess-1".into(),
            project_id: "ws-1".into(),
            run_id: Some("run-1".into()),
            board_task_id: Some("task-1".into()),
            plan_task_id: Some("t1".into()),
            worker_profile: Some("coding".into()),
            image: "agent-fleet-api:local".into(),
            command: None,
            args: None,
            env: BTreeMap::from([("FLEET_WORKER_TOKEN".into(), "tok".into())]),
            resources: None,
            network: Some(SandboxNetwork {
                egress_mode: "none".into(),
                allowlist: vec![],
            }),
            ttl_seconds: Some(600),
        };
        FleetSandbox::new("demo-t1", spec)
    }

    fn pod_env_map(pod: &Pod) -> BTreeMap<String, String> {
        pod.spec
            .as_ref()
            .unwrap()
            .containers[0]
            .env
            .as_ref()
            .unwrap()
            .iter()
            .map(|e| (e.name.clone(), e.value.clone().unwrap_or_default()))
            .collect()
    }

    #[test]
    fn pod_defaults_fleet_driver_and_injects_session_token_and_work_dir() {
        let sb = sample_sb();
        let cfg = sample_cfg();
        let pod = build_pod(&sb, "fsb-demo-t1", &cfg);
        let c = &pod.spec.as_ref().unwrap().containers[0];
        assert_eq!(
            c.command.as_ref().unwrap(),
            &vec!["fleet-driver".to_string()]
        );
        let env = pod_env_map(&pod);
        assert_eq!(env.get("FLEET_SESSION_ID").unwrap(), "sess-1");
        assert_eq!(env.get("FLEET_WORKER_TOKEN").unwrap(), "tok");
        assert_eq!(env.get("FLEET_WORK_DIR").unwrap(), "/data/work");
    }

    #[test]
    fn pod_sets_required_driver_callback_and_identity_env() {
        let sb = sample_sb();
        let cfg = sample_cfg();
        let pod = build_pod(&sb, "fsb-demo-t1", &cfg);
        let env = pod_env_map(&pod);
        assert_eq!(
            env.get("FLEET_ORCHESTRATOR_URL").unwrap(),
            &cfg.api_service_url
        );
        assert_eq!(env.get("FLEET_API_BASE").unwrap(), &cfg.api_service_url);
        assert_eq!(env.get("FLEET_PROJECT_ID").unwrap(), "ws-1");
        assert_eq!(env.get("FLEET_BOARD_TASK_ID").unwrap(), "task-1");
        assert_eq!(env.get("FLEET_TASK_ID").unwrap(), "task-1");
    }

    #[test]
    fn pod_drops_all_caps_denies_sa_token_and_runs_non_root() {
        let sb = sample_sb();
        let cfg = sample_cfg();
        let pod = build_pod(&sb, "fsb-demo-t1", &cfg);
        let spec = pod.spec.as_ref().unwrap();
        assert_eq!(spec.automount_service_account_token, Some(false));
        let psc = spec.security_context.as_ref().unwrap();
        assert_eq!(psc.run_as_non_root, Some(true));
        let sc = spec.containers[0].security_context.as_ref().unwrap();
        assert_eq!(sc.allow_privilege_escalation, Some(false));
        assert_eq!(sc.run_as_non_root, Some(true));
        assert_eq!(sc.run_as_user, Some(10001));
        assert_eq!(
            sc.capabilities.as_ref().and_then(|c| c.drop.clone()),
            Some(vec!["ALL".into()])
        );
    }

    #[test]
    fn spec_env_overrides_injected_defaults_and_empty_image_uses_cfg() {
        let mut sb = sample_sb();
        sb.spec
            .env
            .insert("FLEET_WORK_DIR".into(), "/other".into());
        sb.spec.image = "".into();
        let mut cfg = sample_cfg();
        cfg.worker_image_default = "fleet-worker:from-cfg".into();
        let pod = build_pod(&sb, "fsb-demo-t1", &cfg);
        let c = &pod.spec.as_ref().unwrap().containers[0];
        let env = pod_env_map(&pod);
        assert_eq!(env.get("FLEET_WORK_DIR").unwrap(), "/other");
        assert_eq!(c.image.as_deref(), Some(cfg.worker_image_default.as_str()));
    }

    fn sb_with_net(mode: &str, allowlist: &[&str]) -> FleetSandbox {
        let mut sb = sample_sb();
        sb.spec.network = Some(SandboxNetwork {
            egress_mode: mode.into(),
            allowlist: allowlist.iter().map(|s| (*s).to_string()).collect(),
        });
        sb
    }

    fn egress_rules(np: &NetworkPolicy) -> &[NetworkPolicyEgressRule] {
        np.spec
            .as_ref()
            .and_then(|s| s.egress.as_deref())
            .unwrap_or(&[])
    }

    fn label_value(sel: &Option<LabelSelector>, key: &str) -> Option<String> {
        sel.as_ref()
            .and_then(|s| s.match_labels.as_ref())
            .and_then(|m| m.get(key))
            .cloned()
    }

    fn is_dns_peer(peer: &NetworkPolicyPeer) -> bool {
        label_value(&peer.namespace_selector, "kubernetes.io/metadata.name").as_deref()
            == Some("kube-system")
            && peer.ip_block.is_none()
    }

    fn is_api_peer(peer: &NetworkPolicyPeer, cfg: &OperatorConfig) -> bool {
        label_value(&peer.namespace_selector, "kubernetes.io/metadata.name").as_deref()
            == Some(cfg.api_namespace.as_str())
            && label_value(&peer.pod_selector, "app.kubernetes.io/name").as_deref()
                == Some(cfg.api_pod_label_name.as_str())
            && peer.ip_block.is_none()
    }

    fn rule_ports(rule: &NetworkPolicyEgressRule) -> Vec<(i32, String)> {
        rule.ports
            .as_ref()
            .map(|ps| {
                ps.iter()
                    .filter_map(|p| {
                        let n = match p.port.as_ref()? {
                            IntOrString::Int(i) => *i,
                            _ => return None,
                        };
                        Some((n, p.protocol.clone().unwrap_or_default()))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn has_dns_egress(rules: &[NetworkPolicyEgressRule]) -> bool {
        rules.iter().any(|r| {
            let peers = r.to.as_deref().unwrap_or(&[]);
            let ports = rule_ports(r);
            peers.iter().any(is_dns_peer)
                && ports.iter().any(|(p, proto)| *p == 53 && proto == "UDP")
                && ports.iter().any(|(p, proto)| *p == 53 && proto == "TCP")
        })
    }

    fn has_api_egress(rules: &[NetworkPolicyEgressRule], cfg: &OperatorConfig) -> bool {
        rules.iter().any(|r| {
            let peers = r.to.as_deref().unwrap_or(&[]);
            let ports = rule_ports(r);
            peers.iter().any(|p| is_api_peer(p, cfg))
                && ports.iter().any(|(p, proto)| *p == 8080 && proto == "TCP")
        })
    }

    fn ip_block_cidrs(rules: &[NetworkPolicyEgressRule]) -> Vec<String> {
        let mut out = Vec::new();
        for r in rules {
            for peer in r.to.as_deref().unwrap_or(&[]) {
                if let Some(block) = &peer.ip_block {
                    out.push(block.cidr.clone());
                    if let Some(except) = &block.except {
                        out.extend(except.iter().cloned());
                    }
                }
            }
        }
        out
    }

    fn peer_identity_strings(peer: &NetworkPolicyPeer) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(block) = &peer.ip_block {
            out.push(block.cidr.clone());
            if let Some(except) = &block.except {
                out.extend(except.iter().cloned());
            }
        }
        for sel in [&peer.namespace_selector, &peer.pod_selector] {
            if let Some(s) = sel {
                if let Some(labels) = &s.match_labels {
                    out.extend(labels.keys().cloned());
                    out.extend(labels.values().cloned());
                }
                if let Some(exprs) = &s.match_expressions {
                    for e in exprs {
                        out.push(e.key.clone());
                        if let Some(vals) = &e.values {
                            out.extend(vals.iter().cloned());
                        }
                    }
                }
            }
        }
        out
    }

    fn assert_no_open_egress(rules: &[NetworkPolicyEgressRule]) {
        for r in rules {
            let to = r.to.as_ref();
            assert!(
                to.map(|peers| !peers.is_empty()).unwrap_or(false),
                "omitted or empty egress.to allows all destinations: {r:?}"
            );
        }
    }

    fn assert_dns_plus_api_only(np: &NetworkPolicy, cfg: &OperatorConfig) {
        let spec = np.spec.as_ref().expect("NetworkPolicy spec");
        assert_eq!(
            spec.ingress.as_ref().map(|i| i.len()),
            Some(0),
            "ingress must stay deny-all"
        );
        let types = spec.policy_types.as_deref().unwrap_or(&[]);
        assert!(types.iter().any(|t| t == "Egress"), "{types:?}");
        assert!(types.iter().any(|t| t == "Ingress"), "{types:?}");

        let rules = egress_rules(np);
        assert_no_open_egress(rules);
        assert!(
            has_dns_egress(rules),
            "missing kube-system DNS :53 TCP/UDP: {rules:?}"
        );
        assert!(
            has_api_egress(rules, cfg),
            "missing control-plane API peer :8080/TCP: {rules:?}"
        );
        assert!(
            ip_block_cidrs(rules).is_empty(),
            "none/dns_only (default) must not add IPBlocks; got {:?}",
            ip_block_cidrs(rules)
        );
        for r in rules {
            for peer in r.to.as_deref().unwrap_or(&[]) {
                assert!(
                    is_dns_peer(peer) || is_api_peer(peer, cfg),
                    "none/dns_only egress peer must be DNS or control-plane API, got {peer:?}"
                );
            }
        }
    }

    fn assert_hostname_not_a_peer(rules: &[NetworkPolicyEgressRule], host: &str) {
        for r in rules {
            for peer in r.to.as_deref().unwrap_or(&[]) {
                for id in peer_identity_strings(peer) {
                    assert_ne!(
                        id, host,
                        "hostname {host:?} must not become an egress peer ({id:?})"
                    );
                    assert!(
                        !id.contains(host),
                        "hostname {host:?} must not appear in egress peer identity {id:?}"
                    );
                }
            }
        }
    }

    /// Docs §13: `none` / `dns_only` (default) = DNS + control-plane API only.
    /// `allowlist` adds listed CIDRs, not hostnames. A hostname string must
    /// not become an egress peer.
    #[test]
    fn network_policy_allowlist_cidrs_not_hostnames() {
        let cfg = sample_cfg();
        let mixed = [
            "10.0.0.0/8",
            "github.com",
            "172.18.0.1/32",
            "host.k3d.internal",
            "2001:db8::/32",
        ];
        let cidrs = ["10.0.0.0/8", "172.18.0.1/32", "2001:db8::/32"];
        let hosts = ["github.com", "host.k3d.internal"];

        for mode in ["none", "dns_only", ""] {
            let np = build_network_policy(&sb_with_net(mode, &mixed), "fsb-demo-t1", &cfg);
            assert_dns_plus_api_only(&np, &cfg);
            for host in hosts {
                assert_hostname_not_a_peer(egress_rules(&np), host);
            }
        }

        let mut default_sb = sample_sb();
        default_sb.spec.network = None;
        assert_dns_plus_api_only(
            &build_network_policy(&default_sb, "fsb-demo-t1", &cfg),
            &cfg,
        );

        // Unknown mode is not a hole: only `allowlist` adds CIDRs.
        let unknown = build_network_policy(&sb_with_net("all", &mixed), "fsb-demo-t1", &cfg);
        assert_dns_plus_api_only(&unknown, &cfg);

        let np = build_network_policy(&sb_with_net("allowlist", &mixed), "fsb-demo-t1", &cfg);
        let spec = np.spec.as_ref().expect("NetworkPolicy spec");
        assert_eq!(spec.ingress.as_ref().map(|i| i.len()), Some(0));
        let rules = egress_rules(&np);
        assert_no_open_egress(rules);
        assert!(has_dns_egress(rules), "allowlist keeps DNS: {rules:?}");
        assert!(
            has_api_egress(rules, &cfg),
            "allowlist keeps control-plane API: {rules:?}"
        );
        let mut got = ip_block_cidrs(rules);
        got.sort();
        let mut want = cidrs.map(str::to_string).to_vec();
        want.sort();
        assert_eq!(
            got, want,
            "allowlist must add listed CIDRs only (not hostnames)"
        );
        for host in hosts {
            assert_hostname_not_a_peer(rules, host);
        }
        for r in rules {
            for peer in r.to.as_deref().unwrap_or(&[]) {
                if let Some(block) = &peer.ip_block {
                    assert!(
                        cidrs.contains(&block.cidr.as_str()),
                        "allowlist IPBlock must be a listed CIDR, got {}",
                        block.cidr
                    );
                } else {
                    assert!(
                        is_dns_peer(peer) || is_api_peer(peer, &cfg),
                        "allowlist non-CIDR peer must be DNS or control-plane API, got {peer:?}"
                    );
                }
            }
        }

        let hosts_only =
            build_network_policy(&sb_with_net("allowlist", &hosts), "fsb-demo-t1", &cfg);
        assert_dns_plus_api_only(&hosts_only, &cfg);
        for host in hosts {
            assert_hostname_not_a_peer(egress_rules(&hosts_only), host);
        }
    }

    fn sample_cfg() -> OperatorConfig {
        OperatorConfig {
            worker_image_default: "agent-fleet-api:local".into(),
            api_service_url: "http://api.agent-fleet.svc:8080".into(),
            api_namespace: "agent-fleet".into(),
            api_pod_label_name: "fleet-api".into(),
        }
    }

    fn sandbox_created_ago(ttl_seconds: Option<i64>, age_secs: i64) -> FleetSandbox {
        let mut sb = sample_sb();
        sb.spec.ttl_seconds = ttl_seconds;
        sb.metadata.creation_timestamp =
            Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
                Utc::now() - chrono::Duration::seconds(age_secs),
            ));
        sb
    }

    fn with_running_phase(mut sb: FleetSandbox) -> FleetSandbox {
        sb.status = Some(FleetSandboxStatus {
            phase: Some("Running".into()),
            pod_name: Some(pod_name_for(&sb.name_any())),
            ..Default::default()
        });
        sb
    }

    #[test]
    fn ttl_seconds_exceeded_deletes_the_cr() {
        // Docs §13 / crd hard-lifetime: Running age>=ttl is DeleteCr, not
        // Continue/create-pod or leftover-Running remapped to Exited.
        let at_boundary = with_running_phase(sandbox_created_ago(Some(600), 600));
        assert_eq!(
            apply_decision(&at_boundary),
            ApplyDecision::DeleteCr,
            "age==ttl must DeleteCr, not Continue/create-pod/patch-Running"
        );

        let past = with_running_phase(sandbox_created_ago(Some(600), 601));
        assert_eq!(
            apply_decision(&past),
            ApplyDecision::DeleteCr,
            "age>ttl must DeleteCr, not Continue/create-pod/patch-Running"
        );

        let still_fresh = with_running_phase(sandbox_created_ago(Some(600), 1));
        assert_eq!(
            apply_decision(&still_fresh),
            ApplyDecision::Continue,
            "age<ttl must not delete"
        );
    }

    #[test]
    fn apply_decision_uses_creation_timestamp_not_started_at() {
        // crd.rs: hard lifetime from create, not started_at.
        let mut sb = with_running_phase(sandbox_created_ago(Some(600), 1));
        sb.status.as_mut().unwrap().started_at =
            Some((Utc::now() - chrono::Duration::seconds(10_000)).to_rfc3339());
        assert_eq!(
            apply_decision(&sb),
            ApplyDecision::Continue,
            "started_at 10_000s ago with creation 1s ago must Continue"
        );
    }

    #[test]
    fn apply_decision_without_creation_timestamp_is_continue() {
        let mut sb = with_running_phase(sample_sb());
        sb.spec.ttl_seconds = Some(600);
        sb.metadata.creation_timestamp = None;
        assert_eq!(
            apply_decision(&sb),
            ApplyDecision::Continue,
            "missing creation_timestamp must Continue, not DeleteCr"
        );
    }
}
