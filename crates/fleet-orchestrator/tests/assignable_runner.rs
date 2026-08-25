//! `select_assignable_runner` / `pick_assignable_runner` wrap the host picker.
//! Isolation is in-memory until A3b; default Native, Sandboxed not assignable.
//! Cloud / Partners / public-cloud are not a second spawn path.

use fleet_orchestrator::store::{
    AssignableHost, HostRuntime, RunnerIsolation, RunnerKind, pick_assignable_host,
    pick_assignable_runner,
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
    }
}

#[test]
fn default_isolation_is_native() {
    let h = host(true, true, true, true, 4);
    assert_eq!(h.isolation, RunnerIsolation::Native);
    assert!(h.isolation.is_assignable());
    assert!(!RunnerIsolation::Sandboxed.is_assignable());
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

#[test]
fn sandboxed_is_not_picked() {
    let mut sandboxed = host(true, true, true, true, 4);
    sandboxed.isolation = RunnerIsolation::Sandboxed;
    assert!(pick_assignable_runner(&[sandboxed.clone()], &HashMap::new()).is_none());
    // host picker has no isolation filter; runner wrapper is the gate
    assert!(pick_assignable_host(&[sandboxed], &HashMap::new()).is_some());
}

#[test]
fn skips_sandboxed_and_picks_following_native() {
    let mut sandboxed = host(true, true, true, true, 4);
    sandboxed.isolation = RunnerIsolation::Sandboxed;
    let native = host(true, false, true, false, 2);
    let chosen = pick_assignable_runner(&[sandboxed, native.clone()], &HashMap::new()).unwrap();
    assert_eq!(chosen.id(), native.id);
    assert_eq!(chosen.host.isolation, RunnerIsolation::Native);
    assert_eq!(chosen.runtime, HostRuntime::Host);
}

#[test]
fn only_self_hosted_native_kind_is_assignable() {
    assert!(RunnerKind::SelfHostedNative.is_assignable());
    assert_eq!(
        RunnerKind::SelfHostedNative.isolation(),
        RunnerIsolation::Native
    );
    for kind in [
        RunnerKind::SelfHostedSandbox,
        RunnerKind::PublicCloud,
        RunnerKind::FleetCloud,
        RunnerKind::Partners,
    ] {
        assert!(!kind.is_assignable(), "{kind:?} must not be assignable");
        assert_eq!(kind.isolation(), RunnerIsolation::Sandboxed, "{kind:?}");
        assert!(
            !kind.isolation().is_assignable(),
            "{kind:?} isolation must not be assignable"
        );
    }
}

#[test]
fn cloud_partners_public_cloud_are_not_chosen() {
    let native = host(true, true, true, true, 4);
    let mut fleet_cloud = host(true, true, true, true, 8);
    fleet_cloud.isolation = RunnerKind::FleetCloud.isolation();
    let mut partners = host(true, true, true, true, 8);
    partners.isolation = RunnerKind::Partners.isolation();
    let mut public_cloud = host(true, true, true, true, 8);
    public_cloud.isolation = RunnerKind::PublicCloud.isolation();

    assert!(
        pick_assignable_runner(
            &[fleet_cloud.clone(), partners.clone(), public_cloud.clone()],
            &HashMap::new()
        )
        .is_none(),
        "cloud / partners / public-cloud are not a spawn path"
    );
    // host picker has no kind filter; runner wrapper is the gate
    assert!(pick_assignable_host(&[fleet_cloud.clone()], &HashMap::new()).is_some());

    let chosen = pick_assignable_runner(
        &[fleet_cloud, partners, public_cloud, native.clone()],
        &HashMap::new(),
    )
    .unwrap();
    assert_eq!(chosen.id(), native.id);
    assert_eq!(chosen.host.isolation, RunnerIsolation::Native);
    assert_eq!(chosen.runtime, HostRuntime::Host);
    assert_eq!(
        chosen,
        pick_assignable_host(&[native], &HashMap::new()).unwrap()
    );
}
