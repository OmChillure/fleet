//! Reconcile FleetSandbox → Pod + NetworkPolicy.

use crate::crd::{
    pod_name_for, FleetSandbox, FleetSandboxStatus, LABEL_PROJECT, LABEL_SANDBOX, LABEL_SESSION,
    LABEL_WORKSPACE, FINALIZER,
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
use kube::runtime::finalizer::{finalizer, Event as FinalizerEvent};
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
            api_service_url: std::env::var("API_SERVICE_URL").unwrap_or_else(|_| {
                "http://api.agent-fleet.svc.cluster.local:8080".into()
            }),
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
    let ns = sb.namespace().ok_or_else(|| Error::Other("missing namespace".into()))?;
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

    if ttl_expired(sb) {
        info!(sandbox = %name, "ttl exceeded; deleting FleetSandbox");
        sbs.delete(&name, &DeleteParams::default()).await?;
        return Ok(Action::await_change());
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

async fn patch_status(api: &Api<FleetSandbox>, name: &str, status: FleetSandboxStatus) -> Result<()> {
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
        env_kv("FLEET_WORKER_PROFILE", sb.spec.worker_profile.as_deref().unwrap_or("generalist")),
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
                    size_limit: sb.spec.resources.as_ref().and_then(|r| {
                        r.ephemeral_storage
                            .as_ref()
                            .map(|s| Quantity(s.clone()))
                    }),
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
    let dns_ports = vec![
        np_port(53, "UDP"),
        np_port(53, "TCP"),
    ];

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
                match_labels: Some(BTreeMap::from([(
                    LABEL_SANDBOX.to_string(),
                    name,
                )])),
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

    #[test]
    fn pod_has_worker_contract_and_security() {
        let sb = sample_sb();
        let cfg = OperatorConfig::from_env();
        let pod = build_pod(&sb, "fsb-demo-t1", &cfg);
        let c = &pod.spec.as_ref().unwrap().containers[0];
        assert_eq!(c.command.as_ref().unwrap(), &vec!["fleet-driver".to_string()]);
        let env: BTreeMap<_, _> = c
            .env
            .as_ref()
            .unwrap()
            .iter()
            .map(|e| (e.name.clone(), e.value.clone().unwrap_or_default()))
            .collect();
        assert_eq!(env.get("FLEET_SESSION_ID").unwrap(), "sess-1");
        assert_eq!(env.get("FLEET_WORKER_TOKEN").unwrap(), "tok");
        assert_eq!(env.get("FLEET_WORK_DIR").unwrap(), "/data/work");
        let sc = c.security_context.as_ref().unwrap();
        assert_eq!(sc.allow_privilege_escalation, Some(false));
        assert_eq!(sc.run_as_user, Some(10001));
    }

    #[test]
    fn network_policy_denies_ingress_and_limits_egress() {
        let sb = sample_sb();
        let cfg = OperatorConfig {
            worker_image_default: "agent-fleet-api:local".into(),
            api_service_url: "http://api.agent-fleet.svc:8080".into(),
            api_namespace: "agent-fleet".into(),
            api_pod_label_name: "fleet-api".into(),
        };
        let np = build_network_policy(&sb, "fsb-demo-t1", &cfg);
        let spec = np.spec.as_ref().unwrap();
        assert_eq!(spec.ingress.as_ref().unwrap().len(), 0);
        assert!(spec.egress.as_ref().unwrap().len() >= 2);
        assert_eq!(np.metadata.name.as_deref(), Some("fsb-demo-t1-net"));
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
        sb.metadata.creation_timestamp = Some(k8s_openapi::apimachinery::pkg::apis::meta::v1::Time(
            Utc::now() - chrono::Duration::seconds(age_secs),
        ));
        sb
    }

    #[test]
    fn ttl_expired_deletes_sandbox_session() {
        // apply() deletes the FleetSandbox CR when this is true; kube then
        // runs cleanup() which deletes the session pod + NetworkPolicy.
        let sb = sandbox_created_ago(Some(600), 601);
        assert!(ttl_expired(&sb));
        assert_eq!(sb.spec.session_id, "sess-1");
        assert_eq!(pod_name_for(&sb.name_any()), "fsb-demo-t1");

        let at_boundary = sandbox_created_ago(Some(600), 600);
        assert!(ttl_expired(&at_boundary));
    }

    #[test]
    fn ttl_missing_does_not_delete_early() {
        let mut no_ttl = sandbox_created_ago(None, 10_000);
        assert!(!ttl_expired(&no_ttl));

        no_ttl.spec.ttl_seconds = Some(0);
        assert!(!ttl_expired(&no_ttl));

        no_ttl.spec.ttl_seconds = Some(-1);
        assert!(!ttl_expired(&no_ttl));

        let mut no_created = sample_sb();
        no_created.spec.ttl_seconds = Some(1);
        no_created.metadata.creation_timestamp = None;
        assert!(!ttl_expired(&no_created));

        let still_fresh = sandbox_created_ago(Some(600), 1);
        assert!(!ttl_expired(&still_fresh));

        let one_second_left = sandbox_created_ago(Some(600), 599);
        assert!(!ttl_expired(&one_second_left));
    }

    #[test]
    fn ttl_finalizer_cleanup_targets_pod_and_network_policy() {
        let sb = sample_sb();
        let name = sb.name_any();
        let pod_name = pod_name_for(&name);
        let np_name = network_policy_name(&pod_name);
        assert_eq!(FINALIZER, "fleet.agentfleet.io/sandbox-cleanup");
        assert_eq!(pod_name, "fsb-demo-t1");
        assert_eq!(np_name, "fsb-demo-t1-net");

        let cfg = sample_cfg();
        let desired_pod = build_pod(&sb, &pod_name, &cfg);
        let desired_np = build_network_policy(&sb, &pod_name, &cfg);
        assert_eq!(desired_pod.metadata.name.as_deref(), Some(pod_name.as_str()));
        assert_eq!(desired_np.metadata.name.as_deref(), Some(np_name.as_str()));
    }
}
