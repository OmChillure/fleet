//! Host selection and spawn payload shape (no Postgres).

use fleet_orchestrator::sandbox::remote::{
    BACKEND, host_id_from_handle, runtime_handle, spawn_payload,
};
use fleet_orchestrator::sandbox::{RuntimeBuildOpts, SpawnRequest, build_runtime};
use fleet_orchestrator::store::{
    AssignableHost, HostRuntime, RunnerIsolation, host_at_capacity, pick_assignable_host,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
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
    }
}

fn sample_req(token: &str) -> SpawnRequest {
    SpawnRequest {
        session_id: Uuid::new_v4(),
        project_id: Uuid::new_v4(),
        run_id: Uuid::new_v4(),
        board_task_id: Uuid::new_v4(),
        plan_task_id: "t-plan".into(),
        worker_profile: "coding".into(),
        worker_token: token.into(),
        work_dir: PathBuf::from("/tmp/fleet-host-pool"),
        env: HashMap::from([
            ("FLEET_WORKER_TOKEN".into(), token.into()),
            ("FLEET_SESSION_ID".into(), "sess".into()),
            ("FLEET_WORK_DIR".into(), "/tmp/fleet-host-pool".into()),
        ]),
        timeout: Duration::from_secs(1800),
        task_title: Some("title".into()),
        task_summary: None,
        task_spec: Some("do the work".into()),
        persona_name: None,
        persona_md: None,
        agent_backend: None,
        agent_model: None,
    }
}

#[test]
fn pick_uses_host_in_v1_even_when_container_advertised() {
    let both = host(true, true, true, true, 4);
    let chosen = pick_assignable_host(&[both.clone()], &HashMap::new()).unwrap();
    assert_eq!(chosen.runtime, HostRuntime::Host);
    assert_eq!(chosen.runtime.as_str(), "host");

    let container_only = host(true, true, false, true, 4);
    assert!(
        pick_assignable_host(&[container_only], &HashMap::new()).is_none(),
        "container/k3d is not a v1 executor"
    );

    let host_only = host(true, true, true, false, 4);
    let chosen = pick_assignable_host(&[host_only], &HashMap::new()).unwrap();
    assert_eq!(chosen.runtime, HostRuntime::Host);
}

#[test]
fn pick_skips_host_when_not_advertised_and_no_container() {
    let no_adv_host = host(true, false, false, false, 4);
    assert!(pick_assignable_host(&[no_adv_host], &HashMap::new()).is_none());
}

#[test]
fn pick_skips_full_host_and_disallowed() {
    let full = host(true, true, true, true, 2);
    let denied = host(false, false, true, true, 4);
    let no_adv_host = host(true, false, false, false, 4);
    let ok = host(true, false, true, false, 4);
    let mut inflight = HashMap::new();
    inflight.insert(full.id, 2);
    let chosen = pick_assignable_host(&[full, denied, no_adv_host, ok.clone()], &inflight).unwrap();
    assert_eq!(chosen.id(), ok.id);
    assert_eq!(chosen.runtime, HostRuntime::Host);
    let over = host(true, true, true, true, 1024);
    assert!(host_at_capacity(256, &over));
}

#[test]
fn pick_skips_when_advertised_cap_reached() {
    let mut capped = host(true, true, true, true, 8);
    capped.advertised_max_concurrent = Some(2);
    let mut inflight = HashMap::new();
    inflight.insert(capped.id, 2);
    assert!(host_at_capacity(2, &capped));
    assert!(pick_assignable_host(&[capped], &inflight).is_none());
}

#[test]
fn spawn_payload_shape_matches_contract() {
    let req = sample_req("plain-token");
    let host_id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
    let p = spawn_payload(&req, "http://orch.example", "host", 1800, host_id);
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(v["kind"], "spawn_session");
    assert_eq!(v["session_id"], req.session_id.to_string());
    assert_eq!(v["project_id"], req.project_id.to_string());
    assert_eq!(v["run_id"], req.run_id.to_string());
    assert_eq!(v["board_task_id"], req.board_task_id.to_string());
    assert_eq!(v["plan_task_id"], "t-plan");
    assert_eq!(v["worker_profile"], "coding");
    assert_eq!(v["worker_token"], "plain-token");
    assert_eq!(v["orchestrator_url"], "http://orch.example");
    assert_eq!(v["runtime"], "host");
    assert_eq!(v["task_title"], "title");
    assert_eq!(v["task_spec"], "do the work");
    assert_eq!(v["timeout_s"], 1800);
    assert_eq!(v["host_id"], host_id.to_string());
    assert!(v["repo_url"].is_null());
    assert!(v["git_ref"].is_null());
    assert_eq!(v["env"]["FLEET_WORKER_TOKEN"], "plain-token");
    assert_eq!(v["env"]["FLEET_ORCHESTRATOR_URL"], "http://orch.example");
    assert!(v["env"].get("FLEET_WORK_DIR").is_none());
}

#[test]
fn remote_is_not_a_sandbox_backend_env_value() {
    let opts = RuntimeBuildOpts {
        backend: "remote".into(),
        stub_duration: Duration::from_secs(1),
        data_dir: PathBuf::from("/tmp"),
        worker_argv: vec![],
    };
    let err = match build_runtime(&opts) {
        Ok(_) => panic!("remote must not be a SANDBOX_BACKEND"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("unknown sandbox backend"), "{err}");
    assert_eq!(BACKEND, "remote");
    let id = Uuid::new_v4();
    assert_eq!(host_id_from_handle(&runtime_handle(id)), Some(id));
}
