//! Host selection (no Postgres).

use fleet_orchestrator::store::{
    pick_assignable_host, pick_assignable_runner, AssignableHost, RunnerIsolation,
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
fn pick_skips_host_when_not_advertised_and_no_container() {
    let no_adv_host = host(true, false, false, false, 4);
    assert!(pick_assignable_host(&[no_adv_host], &HashMap::new()).is_none());
}

#[test]
fn pick_lowest_utilization_not_list_order() {
    // Product: among hosts that meet isolation, pick lowest load/cap.
    // A is 3/4 and listed first; B is 0/4. Must pick B.
    let busy = host(true, false, true, false, 4);
    let idle = host(true, false, true, false, 4);
    let mut inflight = HashMap::new();
    inflight.insert(busy.id, 3);
    let chosen = pick_assignable_host(&[busy.clone(), idle.clone()], &inflight)
        .expect("idle host is under capacity");
    assert_eq!(
        chosen.id(),
        idle.id,
        "must pick lowest load/cap, not the first under-capacity host in list order"
    );
    let chosen_rev = pick_assignable_host(&[idle.clone(), busy], &inflight).unwrap();
    assert_eq!(chosen_rev.id(), idle.id, "order must not change the winner");
}

#[test]
fn pick_at_advertised_cap_selects_other_native() {
    // Native at advertised max_jobs=1 is full even when plane max is 8.
    // Pick the other Native; never a Sandboxed placeholder.
    let mut at_cap = host(true, false, true, false, 8);
    at_cap.advertised_max_concurrent = Some(1);
    let mut sandboxed = host(true, false, true, false, 8);
    sandboxed.isolation = RunnerIsolation::Sandboxed;
    let free = host(true, false, true, false, 8);
    let mut inflight = HashMap::new();
    inflight.insert(at_cap.id, 1);

    let chosen = pick_assignable_runner(
        &[at_cap.clone(), sandboxed.clone(), free.clone()],
        &inflight,
    )
    .expect("under-cap native B must be picked");
    assert_eq!(chosen.id(), free.id);
    assert_eq!(chosen.host.isolation, RunnerIsolation::Native);

    let chosen_rev = pick_assignable_runner(&[free.clone(), sandboxed, at_cap], &inflight).unwrap();
    assert_eq!(
        chosen_rev.id(),
        free.id,
        "list order must not pick the at-cap host or a sandboxed box"
    );
}

#[test]
fn pick_advertised_zero_is_not_unlimited() {
    // Some(0) clamps to 1: first job is assignable; inflight 1 is full.
    let mut zero = host(true, false, true, false, 8);
    zero.advertised_max_concurrent = Some(0);
    let chosen = pick_assignable_host(&[zero.clone()], &HashMap::new())
        .expect("advertised 0 still allows the first job (clamp 1)");
    assert_eq!(chosen.id(), zero.id);
    let mut inflight = HashMap::new();
    inflight.insert(zero.id, 1);
    assert!(
        pick_assignable_host(&[zero], &inflight).is_none(),
        "advertised 0 at inflight 1 is full; must not treat 0 as unlimited"
    );
}

#[test]
fn pick_advertised_negative_is_not_unlimited() {
    // Some(-1) clamps to 1 (1..=256), not plane-cap / unlimited.
    let mut neg = host(true, false, true, false, 8);
    neg.advertised_max_concurrent = Some(-1);
    let chosen = pick_assignable_host(&[neg.clone()], &HashMap::new())
        .expect("advertised -1 still allows the first job (clamp 1)");
    assert_eq!(chosen.id(), neg.id);
    let mut inflight = HashMap::new();
    inflight.insert(neg.id, 1);
    assert!(
        pick_assignable_host(&[neg], &inflight).is_none(),
        "advertised -1 at inflight 1 is full; must not treat negative as unlimited"
    );
}
