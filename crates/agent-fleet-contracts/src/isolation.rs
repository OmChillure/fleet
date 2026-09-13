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

/// Host part of a URL. Understands any `scheme://` authority
/// (`http`, `https`, `postgres`, `redis`, …), including `[::1]` and `user@host`.
/// Empty if the string has no host.
pub fn host_from_url(url: &str) -> &str {
    let rest = match url.find("://") {
        Some(i) => &url[i + 3..],
        None => url,
    };
    let authority = rest.split('/').next().unwrap_or(rest);
    let hostport = authority
        .rsplit_once('@')
        .map(|(_, h)| h)
        .unwrap_or(authority);
    if let Some(inside) = hostport.strip_prefix('[') {
        return inside.split(']').next().unwrap_or("");
    }
    if hostport.starts_with("::") {
        return hostport.split('%').next().unwrap_or(hostport);
    }
    hostport
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(hostport)
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
/// Also drop values that are loopback URLs (127/8, localhost, ::1) so
/// guests do not inherit host-only secrets or endpoints.
pub fn strip_isolated_guest_env(env: &mut std::collections::HashMap<String, String>) {
    env.retain(|k, v| !is_host_path_override(k, v) && !url_is_loopback(v));
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

/// Weakest offered class that still meets `need`.
///
/// Empty offer or nothing ≥ need → `None` (do not assign; do not fall back).
pub fn pick_isolation(need: IsolationClass, offer: &[IsolationClass]) -> Option<IsolationClass> {
    offer.iter().copied().filter(|c| *c >= need).min()
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
        assert_eq!(ExecutorKind::Firecracker.isolation(), IsolationClass::Vm);
    }

    #[test]
    fn loopback_includes_127_slash_8_and_ipv6() {
        assert!(url_is_loopback("http://127.0.0.1:8080"));
        assert!(url_is_loopback("http://127.1.2.3:1"));
        assert!(url_is_loopback("http://localhost:8080/v1"));
        assert!(url_is_loopback("http://[::1]:8080"));
        assert!(url_is_loopback("http://::1"));
        assert!(!url_is_loopback("http://10.0.0.8:8080"));
        assert!(!url_is_loopback(
            "http://api.agent-fleet.svc.cluster.local:8080"
        ));
        assert!(!url_is_loopback("http://192.168.1.1:8080"));
    }

    /// Docs §11: k8s guest callback must not be loopback. Pods cannot reach
    /// the API host's `127/8`, `localhost`, or `::1`. `url_is_loopback` /
    /// `host_is_loopback` are the shared rejectors (not only `127.0.0.1`).
    #[test]
    fn k8s_guest_callback_url_must_not_be_loopback() {
        for host in [
            "127.0.0.1",
            "127.1.2.3",
            "127.42.0.1",
            "127.255.255.255",
            "localhost",
            "LOCALHOST",
            "::1",
            "[::1]",
        ] {
            assert!(
                host_is_loopback(host),
                "k8s callback host {host} is loopback (127/8, localhost, ::1)"
            );
        }
        for url in [
            "http://127.0.0.1:8080",
            "https://127.0.0.1/v1",
            "http://127.1.2.3:1",
            "http://127.42.0.1:6379",
            "http://127.255.255.255:8080",
            "http://localhost:8080/v1",
            "http://LOCALHOST:8080",
            "http://[::1]:8080",
            "http://::1",
            "https://127.0.0.1:443/health",
        ] {
            assert!(
                url_is_loopback(url),
                "k8s callback {url} is loopback; pods cannot reach the API host"
            );
            assert!(
                host_is_loopback(host_from_url(url)),
                "host of {url} ({}) must be loopback",
                host_from_url(url)
            );
        }
        for url in [
            "http://api.agent-fleet.svc.cluster.local:8080",
            "http://10.0.0.8:8080",
            "http://172.18.0.1:8081",
            "http://192.168.1.1:8080",
        ] {
            assert!(
                !url_is_loopback(url),
                "{url} is cluster-reachable, not loopback"
            );
        }
        assert!(!host_is_loopback("10.0.0.8"));
        assert!(!host_is_loopback("172.18.0.1"));
        assert!(!host_is_loopback("192.168.1.1"));
        assert!(!host_is_loopback("api.agent-fleet.svc.cluster.local"));
        assert!(!host_is_loopback("8.8.8.8"));
        assert!(!host_is_loopback("126.0.0.1"));
        assert!(!host_is_loopback("128.0.0.1"));
    }

    #[test]
    fn host_from_url_strips_userinfo() {
        assert_eq!(
            host_from_url("http://user:pass@127.0.0.1:8080"),
            "127.0.0.1"
        );
        assert_eq!(host_from_url("http://a@b:c@127.0.0.1:8080"), "127.0.0.1");
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

    #[test]
    fn strip_isolated_guest_env_drops_host_path_bins() {
        let mut env = std::collections::HashMap::from([
            ("FLEET_GROK_BIN".into(), "/opt/plane/grok".into()),
            ("GROK_BIN".into(), "/usr/local/bin/grok".into()),
            (
                "fleet_mock_acp_bin".into(),
                "/secret/host/mock-acp-agent".into(),
            ),
            ("FLEET_DRIVER_CMD".into(), "/opt/plane/fleet-driver".into()),
            ("FLEET_ACP_BIN".into(), "mock-acp-agent".into()),
            ("FLEET_HARNESS".into(), "mock".into()),
        ]);
        strip_isolated_guest_env(&mut env);
        assert!(!env.contains_key("FLEET_GROK_BIN"));
        assert!(!env.contains_key("GROK_BIN"));
        assert!(!env.contains_key("fleet_mock_acp_bin"));
        assert!(!env.contains_key("FLEET_DRIVER_CMD"));
        assert_eq!(
            env.get("FLEET_ACP_BIN").map(String::as_str),
            Some("mock-acp-agent")
        );
        assert_eq!(env.get("FLEET_HARNESS").map(String::as_str), Some("mock"));
    }

    #[test]
    fn strip_isolated_guest_env_keeps_relative_cmd() {
        let mut env = std::collections::HashMap::from([
            ("FLEET_DRIVER_CMD".into(), "fleet-driver".into()),
        ]);
        strip_isolated_guest_env(&mut env);
        assert_eq!(
            env.get("FLEET_DRIVER_CMD").map(String::as_str),
            Some("fleet-driver")
        );
    }

    #[test]
    fn strip_isolated_guest_env_drops_loopback_urls() {
        let mut env = std::collections::HashMap::from([
            (
                "DATABASE_URL".into(),
                "postgres://127.0.0.1:5432/fleet".into(),
            ),
            ("FLEET_API_BASE".into(), "http://127.1.2.3:8080".into()),
            ("HTTP_PROXY".into(), "http://localhost:3128".into()),
            ("FLEET_CONTROL_PLANE_URL".into(), "http://[::1]:8080".into()),
            ("REDIS_URL".into(), "redis://127.42.0.1:6379/0".into()),
            (
                "FLEET_ORCHESTRATOR_URL".into(),
                "http://api.agent-fleet.svc.cluster.local:8080".into(),
            ),
            (
                "FLEET_REPO_URL".into(),
                "https://github.com/example/repo.git".into(),
            ),
            ("LAN_API".into(), "http://10.0.0.8:8080".into()),
            ("FLEET_WORKER_TOKEN".into(), "tok-plain".into()),
        ]);
        strip_isolated_guest_env(&mut env);
        assert!(!env.contains_key("DATABASE_URL"));
        assert!(!env.contains_key("FLEET_API_BASE"));
        assert!(!env.contains_key("HTTP_PROXY"));
        assert!(!env.contains_key("FLEET_CONTROL_PLANE_URL"));
        assert!(!env.contains_key("REDIS_URL"));
        assert_eq!(
            env.get("FLEET_ORCHESTRATOR_URL").map(String::as_str),
            Some("http://api.agent-fleet.svc.cluster.local:8080")
        );
        assert_eq!(
            env.get("FLEET_REPO_URL").map(String::as_str),
            Some("https://github.com/example/repo.git")
        );
        assert_eq!(
            env.get("LAN_API").map(String::as_str),
            Some("http://10.0.0.8:8080")
        );
        assert_eq!(
            env.get("FLEET_WORKER_TOKEN").map(String::as_str),
            Some("tok-plain")
        );
    }

    #[test]
    fn strip_isolated_guest_env_drops_loopback_urls_with_userinfo() {
        let mut env = std::collections::HashMap::from([
            (
                "DATABASE_URL".into(),
                "postgres://user:secret@127.0.0.1:5432/fleet".into(),
            ),
            (
                "FLEET_API_BASE".into(),
                "http://user:pass@localhost:8080".into(),
            ),
            ("REDIS_URL".into(), "redis://u:p@[::1]:6379/0".into()),
            (
                "LAN_DB".into(),
                "postgres://user:secret@10.0.0.8:5432/fleet".into(),
            ),
        ]);
        strip_isolated_guest_env(&mut env);
        assert!(!env.contains_key("DATABASE_URL"));
        assert!(!env.contains_key("FLEET_API_BASE"));
        assert!(!env.contains_key("REDIS_URL"));
        assert_eq!(
            env.get("LAN_DB").map(String::as_str),
            Some("postgres://user:secret@10.0.0.8:5432/fleet")
        );
    }
}
