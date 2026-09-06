//! `select_assignable_runner` / `pick_assignable_runner` wrap the host picker.
//! Isolation is in-memory until A3b; default Native, Sandboxed not assignable.
//! Cloud / Partners / public-cloud are not a second spawn path.

use fleet_orchestrator::store::{
    pick_assignable_host, pick_assignable_runner, AssignableHost, HostRuntime, RunnerIsolation,
};
use std::collections::HashMap;
use uuid::Uuid;

fn host(
    allow_host: bool,
    allow_container: bool,
    advertised_host: bool,
    advertised_container: bool,
    max_concurrent: i32,
) -> AssignableHost {
    AssignableHost {
        id: Uuid::new_v4(),
        allow_host,
        allow_container,
        advertised_host,
        advertised_container,
        max_concurrent,
        advertised_max_concurrent: None,
        max_job_timeout_s: 3600,
        isolation: RunnerIsolation::Native,
        last_seen_at: None,
        workspace_id: Uuid::nil(),
        hostname: None,
        addrs: vec![],
    }
}

#[test]
fn wrapper_picks_paired_native_host() {
    let native = host(true, true, true, true, 4);
    let chosen = pick_assignable_runner(&[native.clone()], &HashMap::new()).unwrap();
    assert_eq!(chosen.id(), native.id);
    assert_eq!(chosen.runtime, HostRuntime::Host);
    assert_eq!(chosen.host.isolation, RunnerIsolation::Native);
    assert_eq!(
        chosen,
        pick_assignable_host(&[native], &HashMap::new()).unwrap()
    );
}
