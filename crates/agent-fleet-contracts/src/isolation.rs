//! Isolation class vs executor / placement.
//!
//! `k8s` is **not** an isolation level. A pod is still a container. A VM has
//! its own kernel whether `fleet-host` uses QEMU on the box or KubeVirt.

use serde::{Deserialize, Serialize};

/// What the job is promised (weak → strong).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsolationClass {
    Process,
    Container,
    Vm,
}

impl IsolationClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Process => "process",
            Self::Container => "container",
            Self::Vm => "vm",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "process" | "host" | "native" => Some(Self::Process),
            "container" | "docker" | "pod" => Some(Self::Container),
            "vm" | "virtual_machine" | "firecracker" | "qemu" => Some(Self::Vm),
            // Placement words are not isolation — callers must map them.
            "k8s" | "kubernetes" => None,
            _ => None,
        }
    }
}

/// How this `fleet-host` starts a guest of a given isolation class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorKind {
    Exec,
    Docker,
    Firecracker,
    Qemu,
    KubePod,
    Kubevirt,
}

impl ExecutorKind {
    pub fn isolation(self) -> IsolationClass {
        match self {
            Self::Exec => IsolationClass::Process,
            Self::Docker | Self::KubePod => IsolationClass::Container,
            Self::Firecracker | Self::Qemu | Self::Kubevirt => IsolationClass::Vm,
        }
    }
}

/// Weakest offered class that still meets `need`.
///
/// Empty offer or nothing ≥ need → `None` (do not assign; do not fall back).
/// Host part of an `http(s)://` URL. Empty if the string has no host.
pub fn host_from_url(url: &str) -> &str {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    let rest = rest.split('/').next().unwrap_or(rest);
    if let Some(inside) = rest.strip_prefix('[') {
        return inside.split(']').next().unwrap_or("");
    }
    if rest.starts_with("::") {
        return rest.split('%').next().unwrap_or(rest);
    }
    rest.rsplit_once(':').map(|(h, _)| h).unwrap_or(rest)
}

/// Loopback host: `localhost`, `::1`, or `127.0.0.0/8`.
pub fn host_is_loopback(host: &str) -> bool {
    let h = host.trim().trim_matches(|c| c == '[' || c == ']');
    if h.eq_ignore_ascii_case("localhost") || h == "::1" {
        return true;
    }
    let Some(rest) = h.strip_prefix("127.") else {
        return false;
    };
    let mut n = 0;
    for p in rest.split('.') {
        n += 1;
        if n > 3 || p.parse::<u8>().is_err() {
            return false;
        }
    }
    n == 3
}

/// Orchestrator URL a container/VM guest cannot reach.
pub fn url_is_loopback(url: &str) -> bool {
    host_is_loopback(host_from_url(url))
}

/// Drop host-absolute `*_BIN` / `*_CMD` (and `FLEET_WORK_CMD`) so isolated
/// guests do not exec a path that exists only on the plane or host box.
pub fn strip_isolated_guest_env(env: &mut std::collections::HashMap<String, String>) {
    env.retain(|k, v| !is_host_path_override(k, v));
}

fn is_host_path_override(key: &str, value: &str) -> bool {
    let key = key.to_ascii_uppercase();
    if key == "FLEET_WORK_CMD" {
        return true;
    }
    if key.ends_with("_BIN") || key.ends_with("_CMD") {
        return std::path::Path::new(value).is_absolute();
    }
    false
}

pub fn pick_isolation(
    need: IsolationClass,
    offer: &[IsolationClass],
) -> Option<IsolationClass> {
    offer
        .iter()
        .copied()
        .filter(|c| *c >= need)
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_is_process_lt_container_lt_vm() {
        assert!(IsolationClass::Process < IsolationClass::Container);
        assert!(IsolationClass::Container < IsolationClass::Vm);
    }

    #[test]
    fn k8s_is_not_an_isolation_class() {
        assert_eq!(IsolationClass::parse("k8s"), None);
        assert_eq!(IsolationClass::parse("kubernetes"), None);
        assert_eq!(IsolationClass::parse("vm"), Some(IsolationClass::Vm));
        assert_eq!(IsolationClass::parse("host"), Some(IsolationClass::Process));
    }

    #[test]
    fn pick_cheapest_that_meets_need() {
        let offer = [
            IsolationClass::Process,
            IsolationClass::Container,
            IsolationClass::Vm,
        ];
        assert_eq!(
            pick_isolation(IsolationClass::Process, &offer),
            Some(IsolationClass::Process)
        );
        assert_eq!(
            pick_isolation(IsolationClass::Container, &offer),
            Some(IsolationClass::Container)
        );
        assert_eq!(
            pick_isolation(IsolationClass::Vm, &offer),
            Some(IsolationClass::Vm)
        );
    }

    #[test]
    fn pick_does_not_fall_back_below_need() {
        let docker_only = [IsolationClass::Container];
        assert_eq!(pick_isolation(IsolationClass::Vm, &docker_only), None);
        assert_eq!(pick_isolation(IsolationClass::Process, &[]), None);
    }

    #[test]
    fn kube_pod_is_container_kubevirt_is_vm() {
        assert_eq!(ExecutorKind::KubePod.isolation(), IsolationClass::Container);
        assert_eq!(ExecutorKind::Kubevirt.isolation(), IsolationClass::Vm);
        assert_eq!(
            ExecutorKind::Firecracker.isolation(),
            IsolationClass::Vm
        );
    }

    #[test]
    fn loopback_includes_127_slash_8_and_ipv6() {
        assert!(url_is_loopback("http://127.0.0.1:8080"));
        assert!(url_is_loopback("http://127.1.2.3:1"));
        assert!(url_is_loopback("http://localhost:8080/v1"));
        assert!(url_is_loopback("http://[::1]:8080"));
        assert!(url_is_loopback("http://::1"));
        assert!(!url_is_loopback("http://10.0.0.8:8080"));
        assert!(!url_is_loopback("http://api.agent-fleet.svc.cluster.local:8080"));
        assert!(!url_is_loopback("http://192.168.1.1:8080"));
    }

    #[test]
    fn strip_drops_host_absolute_bins_not_relative() {
        let mut env = std::collections::HashMap::from([
            (
                "FLEET_MOCK_ACP_BIN".into(),
                "/home/viny/.local/bin/mock-acp-agent".into(),
            ),
            ("FLEET_GROK_BIN".into(), "grok".into()),
            ("FLEET_WORK_CMD".into(), "echo hi".into()),
            ("FLEET_MODEL".into(), "llama".into()),
        ]);
        strip_isolated_guest_env(&mut env);
        assert!(!env.contains_key("FLEET_MOCK_ACP_BIN"));
        assert!(!env.contains_key("FLEET_WORK_CMD"));
        assert_eq!(env.get("FLEET_GROK_BIN").map(String::as_str), Some("grok"));
        assert_eq!(env.get("FLEET_MODEL").map(String::as_str), Some("llama"));
    }
}
