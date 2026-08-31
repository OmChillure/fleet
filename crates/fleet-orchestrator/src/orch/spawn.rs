//! Step 6–7 capacity schedule + spawn protocol.

use crate::domain::{WorkerSessionStatus, free_slots_from_db_statuses};
use crate::sandbox::remote::{self, RemoteHostHandle};
use crate::sandbox::{SandboxRuntime, SpawnRequest, fleet_worker_env};
use crate::store::{
    BoardSnapshot, Store, enqueue_remote_spawn, hold_ready_for_host_update,
    select_assignable_host_preferring, workspace_has_updating_host,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

use super::tick::TickError;

/// Catalog: online remote native under cap wins; local `SANDBOX_BACKEND`
/// only when the picker found no host and the plane is taking jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpawnPlacement {
    Remote,
    Local,
    HoldReady,
}

fn spawn_placement(
    has_assignable_remote: bool,
    allow_local_spawn: bool,
    plane_taking_jobs: bool,
    updating_recent: bool,
) -> SpawnPlacement {
    // Mid host-bin swap (`software_updating`, last_seen inside the hold):
    // stay ready. A killed guest is not assignable; must not fall through
    // to the plane process sandbox even when tests enable local spawn.
    if hold_ready_for_host_update(has_assignable_remote, updating_recent) {
        return SpawnPlacement::HoldReady;
    }
    if has_assignable_remote {
        SpawnPlacement::Remote
    } else if allow_local_spawn && plane_taking_jobs {
        SpawnPlacement::Local
    } else {
        SpawnPlacement::HoldReady
    }
}

pub struct SpawnOutcome {
    pub spawned: usize,
    pub spawn_failed: usize,
}

/// Docs §16: Settings git remote is snapshotted at spawn. An already-attached
/// session `repo/` is left alone when the remote later changes; a new spawn
/// clones the current URL. A cleared / empty Settings remote is no clone
/// (prompt-only).
fn git_remote_for_new_spawn(
    session_work: &Path,
    current: Option<&crate::domain::ProjectGitRemote>,
) -> Option<crate::domain::ProjectGitRemote> {
    if session_work.join("repo").exists() {
        return None;
    }
    current.filter(|r| !r.url.trim().is_empty()).cloned()
}

/// Bind the spawn-time Settings remote onto job env (`FLEET_REPO_URL` /
/// `FLEET_GIT_REF`). Already-attached `work_dir/repo` gets nothing so a later
/// Settings change cannot rewrite that session's checkout. Cleared Settings
/// must drop a leftover clone URL — the worker clones whenever `FLEET_REPO_URL`
/// is set.
fn bind_spawn_git_remote(
    req: &mut SpawnRequest,
    current: Option<&crate::domain::ProjectGitRemote>,
) -> Option<crate::domain::ProjectGitRemote> {
    let git = git_remote_for_new_spawn(&req.work_dir, current);
    match &git {
        Some(r) => {
            req.env.insert("FLEET_REPO_URL".into(), r.url.clone());
            req.env.insert("FLEET_GIT_REF".into(), r.git_ref.clone());
        }
        None => {
            req.env.remove("FLEET_REPO_URL");
            req.env.remove("FLEET_GIT_REF");
        }
    }
    git
}

pub async fn schedule_and_spawn(
    store: &Store,
    runtime: &Arc<dyn SandboxRuntime>,
    project_id: Uuid,
    snap: &BoardSnapshot,
    max_agents: i32,
    data_dir: &PathBuf,
    orchestrator_url: &str,
    default_timeout_s: i32,
    fleet_llm: &str,
    fleet_llm_model: &str,
    integrations: crate::sandbox::io::IntegrationHandle,
    allow_local_spawn: bool,
) -> Result<SpawnOutcome, TickError> {
    let active_runs: HashSet<Uuid> = store
        .list_active_run_ids_not_cancelling(project_id)
        .await?
        .into_iter()
        .collect();

    // Docs §8.3 / §15.2: spawning|healthy|stuck|killing each hold a
    // max_agents slot. stuck/killing must not look free or this tick
    // over-spawns.
    let mut free =
        free_slots_from_db_statuses(snap.sessions.iter().map(|s| s.status.as_str()), max_agents);

    let mut ready: Vec<_> = snap
        .tasks
        .iter()
        .filter(|t| t.status == "ready" && active_runs.contains(&t.run_id))
        .collect();
    ready.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.created_at.cmp(&b.created_at))
    });

    let mut outcome = SpawnOutcome {
        spawned: 0,
        spawn_failed: 0,
    };
    let mut plan_llms: HashMap<Uuid, crate::sandbox::agent_loop::WorkerLlm> = HashMap::new();
    let current_git = store
        .get_project_config(project_id)
        .await
        .ok()
        .and_then(|c| c.git_remote());
    let mut local_in_use = snap
        .sessions
        .iter()
        .filter(|s| {
            s.runtime_backend != remote::BACKEND
                && WorkerSessionStatus::from_db(&s.status).is_some_and(|st| st.holds_capacity())
        })
        .count() as i32;

    for task in ready {
        if free <= 0 {
            break;
        }
        // Missing worker_hosts must not fail the tick.
        let plan_hint = store.get_plan_body(task.plan_id).await.ok().flatten();
        let preferred = plan_hint.as_ref().and_then(|p| p.policy.preferred_host_id);
        let mut remote_host =
            select_assignable_host_preferring(store, project_id, preferred).await;
        // Off-box workers cannot heartbeat to a loopback plane URL. Local
        // fleet-dev (this-machine hostname) may still take host-runtime work.
        if let Some(ref chosen) = remote_host {
            if worker_url_is_loopback(orchestrator_url)
                && !chosen_host_is_this_machine(store, chosen.id()).await
            {
                tracing::warn!(
                    url = %orchestrator_url,
                    host_id = %chosen.id(),
                    "refusing remote assign: orchestrator URL is loopback and host is not this machine"
                );
                remote_host = None;
            }
        }
        let updating = workspace_has_updating_host(store, project_id).await;
        if hold_ready_for_host_update(remote_host.is_some(), updating) {
            tracing::info!(
                task_id = %task.id,
                "paired host is applying a host-bin update; leaving task ready"
            );
            continue;
        }
        // No assignable host and local SANDBOX_BACKEND disabled → stay ready.
        if remote_host.is_none() && !allow_local_spawn {
            tracing::info!(
                task_id = %task.id,
                "no assignable host; leaving task ready"
            );
            continue;
        }
        // Plane taking jobs + no remote → in-process SANDBOX_BACKEND.
        if remote_host.is_none() {
            match fleet_db::repos::plane_runners::get_or_default(store.pool(), project_id).await {
                Ok(plane) if !plane.taking_native_jobs() => {
                    tracing::info!(
                        task_id = %task.id,
                        "control plane runner is not taking jobs; leaving task ready"
                    );
                    continue;
                }
                Ok(plane) => {
                    let cap = plane.max_concurrent.clamp(1, 256);
                    if local_in_use >= cap {
                        tracing::info!(
                            task_id = %task.id,
                            local_in_use,
                            cap,
                            "control plane at max concurrent; leaving task ready"
                        );
                        continue;
                    }
                }
                Err(e) => {
                    tracing::debug!(
                        error = %e,
                        project_id = %project_id,
                        "plane runner settings unavailable; no local spawn"
                    );
                    continue;
                }
            }
        }
        // Remote native under cap wins even when tests enable local SANDBOX_BACKEND.
        // `updating` is passed again so a deleted early hold cannot local-spawn.
        let place = spawn_placement(remote_host.is_some(), allow_local_spawn, true, updating);
        let backend = match place {
            SpawnPlacement::Remote => remote::BACKEND,
            SpawnPlacement::Local => runtime.backend_name(),
            SpawnPlacement::HoldReady => continue,
        };
        let assignment = store
            .begin_spawn_assignment(
                task.id,
                project_id,
                task.run_id,
                &task.worker_profile,
                backend,
                None,
            )
            .await?;
        let Some(assignment) = assignment else {
            continue;
        };

        let session_work = data_dir
            .join("projects")
            .join(project_id.to_string())
            .join("sessions")
            .join(assignment.session_id.to_string());
        let _ = tokio::fs::create_dir_all(&session_work).await;

        // Shared empty workspace — agents collaborate here. GitHub/GitLab is an
        // *action* via control-plane tools if the user connected Integrations.
        crate::sandbox::share::ensure_run_workspace(data_dir, project_id, task.run_id, None, None)
            .await;

        crate::sandbox::team::prepare_agent_workspace(data_dir, &session_work, task, snap).await;

        let timeout_s = task.timeout_s.unwrap_or(default_timeout_s).max(1) as u64;
        // Docs §6.7: agency-agents roster files only; workspace personas
        // are stored for later runs and are not consulted at spawn.
        let persona = crate::personas::resolve(&task.worker_profile, data_dir).await;
        let llm = if let Some(cached) = plan_llms.get(&task.plan_id) {
            cached.clone()
        } else {
            let plan = store.get_plan_body(task.plan_id).await.ok().flatten();
            let resolved = crate::sandbox::agent_loop::resolve_worker_llm(
                plan.as_ref().and_then(|p| p.policy.llm_provider.as_deref()),
                plan.as_ref().and_then(|p| p.policy.llm_model.as_deref()),
                fleet_llm,
                fleet_llm_model,
            );
            plan_llms.insert(task.plan_id, resolved.clone());
            resolved
        };
        let mut req = SpawnRequest {
            session_id: assignment.session_id,
            project_id,
            run_id: task.run_id,
            board_task_id: task.id,
            plan_task_id: task.plan_task_id.clone(),
            worker_profile: task.worker_profile.clone(),
            worker_token: assignment.worker_token.clone(),
            work_dir: session_work,
            env: HashMap::new(),
            timeout: Duration::from_secs(timeout_s),
            task_title: Some(task.title.clone()),
            task_summary: Some(task.summary.clone()),
            // Full spec (may include integration context markdown + ```bash fences).
            task_spec: Some(task.spec_md.clone()),
            persona_name: Some(persona.name.clone()),
            persona_md: Some(persona.body.clone()),
            agent_backend: llm.backend.clone(),
            agent_model: llm.model.clone(),
            io: integrations.clone(),
        };
        apply_spawn_env(
            &mut req,
            orchestrator_url,
            timeout_s,
            plan_hint
                .as_ref()
                .and_then(|p| p.policy.reasoning_effort.clone()),
            task.hitl_notes.as_deref(),
        );
        let git_for_spawn = bind_spawn_git_remote(&mut req, current_git.as_ref());
        if let Err(e) = super::session_context::write_session_context(
            &req.work_dir,
            task,
            timeout_s,
            git_for_spawn.as_ref(),
        ) {
            tracing::warn!(
                error = %e,
                path = %req.work_dir.display(),
                "failed to write session task context"
            );
        }

        let work_dir_str = req.work_dir.to_string_lossy().into_owned();
        if place == SpawnPlacement::Remote {
            let Some(chosen) = remote_host else {
                continue;
            };
            if worker_url_is_loopback(orchestrator_url)
                && chosen.runtime == crate::store::HostRuntime::Container
            {
                tracing::warn!(
                    url = %orchestrator_url,
                    host_id = %chosen.id(),
                    "refusing container assign: orchestrator URL is loopback"
                );
                store.complete_spawn_failure(assignment.session_id).await?;
                apply_retry_after_failure(
                    store,
                    task.id,
                    assignment.attempt,
                    task.max_retries,
                    false,
                    crate::domain::ExitReason::SpawnFailed,
                )
                .await?;
                outcome.spawn_failed += 1;
                continue;
            }
            if worker_url_is_loopback(orchestrator_url) {
                tracing::warn!(
                    url = %orchestrator_url,
                    "assigning a paired host while orchestrator URL is loopback; off-box workers cannot heartbeat"
                );
            }
            let handle = RemoteHostHandle::new(chosen.id()).as_str();
            match enqueue_remote_spawn(store, &chosen, &req, orchestrator_url).await {
                Ok(()) => {
                    store
                        .record_remote_enqueued(
                            assignment.session_id,
                            &handle,
                            Some(work_dir_str.as_str()),
                        )
                        .await?;
                    outcome.spawned += 1;
                    free -= 1;
                }
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        task_id = %task.id,
                        host_id = %chosen.id(),
                        "remote spawn enqueue failed"
                    );
                    store.complete_spawn_failure(assignment.session_id).await?;
                    apply_retry_after_failure(
                        store,
                        task.id,
                        assignment.attempt,
                        task.max_retries,
                        false,
                        crate::domain::ExitReason::SpawnFailed,
                    )
                    .await?;
                    outcome.spawn_failed += 1;
                }
            }
            continue;
        }
        match runtime.spawn(req).await {
            Ok(handle) => {
                store
                    .complete_spawn_success(
                        assignment.session_id,
                        task.id,
                        task.run_id,
                        &handle.id,
                        Some(work_dir_str.as_str()),
                    )
                    .await?;
                outcome.spawned += 1;
                local_in_use += 1;
                free -= 1;
            }
            Err(e) => {
                tracing::warn!(error = %e, task_id = %task.id, "spawn failed");
                store.complete_spawn_failure(assignment.session_id).await?;
                apply_retry_after_failure(
                    store,
                    task.id,
                    assignment.attempt,
                    task.max_retries,
                    false,
                    crate::domain::ExitReason::SpawnFailed,
                )
                .await?;
                outcome.spawn_failed += 1;
                // free slot not consumed by failed_spawn (does not hold capacity)
            }
        }
    }
    Ok(outcome)
}

fn worker_url_is_loopback(url: &str) -> bool {
    agent_fleet_contracts::url_is_loopback(url)
}

fn local_hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_default()
}

/// Same-box as the plane: paired hostname matches this process (fleet-dev).
async fn chosen_host_is_this_machine(store: &Store, host_id: Uuid) -> bool {
    let local = local_hostname();
    if local.is_empty() {
        return false;
    }
    match fleet_db::repos::worker_hosts::get(store.pool(), host_id).await {
        Ok(Some(host)) => host
            .hostname
            .as_deref()
            .is_some_and(|h| h.eq_ignore_ascii_case(&local)),
        _ => false,
    }
}

/// FLEET_* job env for local and remote spawn. OAuth / integration secrets
/// stay on the control plane — they are never copied into this map.
fn apply_spawn_env(
    req: &mut SpawnRequest,
    orchestrator_url: &str,
    timeout_s: u64,
    reasoning_effort: Option<String>,
    hitl_notes: Option<&str>,
) {
    req.env = fleet_worker_env(req, orchestrator_url);
    req.env
        .insert("FLEET_TASK_TIMEOUT_S".into(), timeout_s.to_string());
    super::session_context::pass_through_driver_env(&mut req.env);
    // Guest is fleet-driver (ACP). Harness from the plane, else mock.
    if req
        .env
        .get("FLEET_HARNESS")
        .map(|s| s.trim().is_empty())
        .unwrap_or(true)
    {
        let harness = match req.agent_backend.as_deref() {
            Some("grok") => "grok",
            _ => "mock",
        };
        req.env.insert("FLEET_HARNESS".into(), harness.into());
    }
    if let Some(model) = req.agent_model.as_deref().filter(|s| !s.is_empty()) {
        req.env
            .entry("FLEET_MODEL".into())
            .or_insert_with(|| model.to_string());
    }
    if let Some(effort) = reasoning_effort.filter(|s| !s.trim().is_empty()) {
        req.env
            .entry("FLEET_REASONING_EFFORT".into())
            .or_insert(effort);
    }
    if let Some(notes) = hitl_notes {
        req.env.insert("FLEET_HITL_NOTES".into(), notes.to_string());
    }
    // Optional shell override (legacy; fleet-driver ignores this).
    if let Ok(cmd) = std::env::var("FLEET_WORK_CMD")
        && !cmd.trim().is_empty()
    {
        req.env.insert("FLEET_WORK_CMD".into(), cmd);
    }
    // Tokens stay on the CP. Remote/local job env is FLEET_* only.
    req.env.retain(|k, _| k.starts_with("FLEET_"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::remote::spawn_payload;
    use crate::store::{AssignableHost, RunnerIsolation, pick_assignable_runner};

    const SECRET_GITHUB: &str = "ghs_must_not_reach_host";
    const SECRET_ACCESS: &str = "oauth_access_must_not_reach_host";
    const SECRET_CLIENT: &str = "oauth_client_secret_must_not_reach_host";
    const SECRET_CIPHER: &str =
        r#"{"_enc":"v1","nonce":"00ff","ct":"cafebabe_must_not_reach_host"}"#;

    fn sample_req() -> SpawnRequest {
        SpawnRequest {
            session_id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            project_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            run_id: Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
            board_task_id: Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap(),
            plan_task_id: "t1".into(),
            worker_profile: "coding".into(),
            worker_token: "sess-tok".into(),
            work_dir: PathBuf::from("/tmp/fleet-spawn-env-test"),
            env: HashMap::new(),
            timeout: Duration::from_secs(1800),
            task_title: Some("Do it".into()),
            task_summary: Some("summary".into()),
            task_spec: Some("spec".into()),
            persona_name: Some("coding".into()),
            persona_md: Some("be helpful".into()),
            agent_backend: Some("mock".into()),
            agent_model: Some("mock".into()),
            io: crate::sandbox::io::IntegrationHandle::none(),
        }
    }

    fn is_forbidden_secret_key(key: &str) -> bool {
        let lower = key.to_ascii_lowercase().replace('-', "_");
        matches!(
            lower.as_str(),
            "github_token"
                | "gh_token"
                | "access_token"
                | "refresh_token"
                | "client_secret"
                | "github_client_secret"
                | "id_token"
        ) || lower.ends_with("_client_secret")
            || lower.contains("access_token")
    }

    fn assert_remote_job_env_is_fleet_only(env: &HashMap<String, String>, dumped: &str) {
        for (k, v) in env {
            assert!(
                k.starts_with("FLEET_"),
                "remote spawn env must be FLEET_* only, got {k}={v}"
            );
            assert!(
                !is_forbidden_secret_key(k),
                "OAuth/integration secret key must not reach the host: {k}"
            );
        }
        for needle in [SECRET_GITHUB, SECRET_ACCESS, SECRET_CLIENT, SECRET_CIPHER] {
            assert!(
                !dumped.contains(needle),
                "integration/OAuth secret leaked onto the host payload: {needle}"
            );
            for v in env.values() {
                assert!(
                    !v.contains(needle),
                    "integration/OAuth secret leaked into env value {v}"
                );
            }
        }
        for k in env.keys() {
            assert!(
                !is_forbidden_secret_key(k),
                "secret-named key in remote env: {k}"
            );
        }
    }

    fn remote_payload_dump(req: &SpawnRequest) -> (HashMap<String, String>, String) {
        let host_id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        let payload = spawn_payload(req, "http://orch.example:8080", "host", 1800, host_id);
        let dumped = serde_json::to_string(&payload).expect("spawn_payload serializes");
        (payload.env, dumped)
    }

    #[test]
    fn loopback_detects_ipv4_ipv6_and_localhost() {
        assert!(worker_url_is_loopback("http://127.0.0.1:8080"));
        assert!(worker_url_is_loopback("http://localhost:8080/v1"));
        assert!(
            worker_url_is_loopback("http://[::1]:8080"),
            "container guests cannot reach IPv6 loopback of the plane"
        );
        assert!(!worker_url_is_loopback(
            "http://api.agent-fleet.svc.cluster.local:8080"
        ));
        assert!(!worker_url_is_loopback("http://10.0.0.8:8080"));
        assert!(
            worker_url_is_loopback("http://127.1.2.3:8080"),
            "127.0.0.0/8 is loopback; container guests cannot reach it"
        );
    }

    #[test]
    fn spawn_env_from_empty_request_is_fleet_prefix_only() {
        let mut req = sample_req();
        apply_spawn_env(&mut req, "http://orch.example:8080", 1800, None, None);
        assert!(
            req.env.keys().all(|k| k.starts_with("FLEET_")),
            "{:?}",
            req.env
        );
        assert_eq!(req.env.get("FLEET_WORKER_TOKEN").unwrap(), "sess-tok");
        assert_eq!(
            req.env.get("FLEET_ORCHESTRATOR_URL").unwrap(),
            "http://orch.example:8080"
        );
        let (env, dumped) = remote_payload_dump(&req);
        assert_remote_job_env_is_fleet_only(&env, &dumped);
        assert!(!env.contains_key("FLEET_WORK_DIR"));
        assert_eq!(env.get("FLEET_WORKER_TOKEN").unwrap(), "sess-tok");
    }

    #[test]
    fn spawn_env_does_not_copy_process_oauth_or_integration_secrets() {
        let keys = [
            "GITHUB_TOKEN",
            "access_token",
            "client_secret",
            "ACCESS_TOKEN",
            "REFRESH_TOKEN",
            "GITHUB_CLIENT_SECRET",
        ];
        let prev: Vec<(String, Option<String>)> = keys
            .iter()
            .map(|k| ((*k).to_string(), std::env::var(k).ok()))
            .collect();
        unsafe {
            std::env::set_var("GITHUB_TOKEN", SECRET_GITHUB);
            std::env::set_var("access_token", SECRET_ACCESS);
            std::env::set_var("client_secret", SECRET_CLIENT);
            std::env::set_var("ACCESS_TOKEN", SECRET_ACCESS);
            std::env::set_var("REFRESH_TOKEN", SECRET_ACCESS);
            std::env::set_var("GITHUB_CLIENT_SECRET", SECRET_CLIENT);
        }
        let mut req = sample_req();
        apply_spawn_env(&mut req, "http://orch.example:8080", 1800, None, None);
        let (env, dumped) = remote_payload_dump(&req);
        for (k, old) in prev {
            match old {
                Some(v) => unsafe { std::env::set_var(&k, v) },
                None => unsafe { std::env::remove_var(&k) },
            }
        }
        assert_remote_job_env_is_fleet_only(&req.env, &dumped);
        assert_remote_job_env_is_fleet_only(&env, &dumped);
        for secret_key in [
            "GITHUB_TOKEN",
            "access_token",
            "client_secret",
            "ACCESS_TOKEN",
            "REFRESH_TOKEN",
            "GITHUB_CLIENT_SECRET",
        ] {
            assert!(
                !req.env.contains_key(secret_key) && !env.contains_key(secret_key),
                "process env {secret_key} must stay on the control plane"
            );
        }
    }

    #[test]
    fn fleet_worker_env_and_spawn_payload_do_not_copy_integration_secrets() {
        let mut req = sample_req();
        req.env.insert("GITHUB_TOKEN".into(), SECRET_GITHUB.into());
        req.env.insert("access_token".into(), SECRET_ACCESS.into());
        req.env.insert("client_secret".into(), SECRET_CLIENT.into());
        req.env
            .insert("INTEGRATION_CREDENTIALS".into(), SECRET_CIPHER.into());
        req.env
            .insert("GITHUB_CLIENT_SECRET".into(), SECRET_CLIENT.into());
        req.env.insert("REFRESH_TOKEN".into(), SECRET_ACCESS.into());

        apply_spawn_env(&mut req, "http://orch.example:8080", 1800, None, None);
        let (payload_env, dumped) = remote_payload_dump(&req);

        for env in [&req.env, &payload_env] {
            assert_remote_job_env_is_fleet_only(env, &dumped);
            for secret_key in [
                "GITHUB_TOKEN",
                "access_token",
                "client_secret",
                "INTEGRATION_CREDENTIALS",
                "GITHUB_CLIENT_SECRET",
                "REFRESH_TOKEN",
            ] {
                assert!(
                    !env.contains_key(secret_key),
                    "fleet_worker_env/spawn_payload must not copy {secret_key} onto the host: {env:?}"
                );
            }
        }
        assert!(
            !dumped.contains(SECRET_CIPHER),
            "integration ciphertext must stay on the control plane"
        );
    }

    fn native_host(max_concurrent: i32) -> AssignableHost {
        AssignableHost {
            id: Uuid::new_v4(),
            allow_host: true,
            allow_container: false,
            advertised_host: true,
            advertised_container: false,
            max_concurrent,
            advertised_max_concurrent: None,
            max_job_timeout_s: 3600,
            isolation: RunnerIsolation::Native,
            last_seen_at: None,
            workspace_id: Uuid::nil(),
        }
    }

    #[test]
    fn spawn_prefers_online_remote_native_over_local_sandbox_backend() {
        let free = native_host(4);
        let mut sandboxed = native_host(8);
        sandboxed.isolation = RunnerIsolation::Sandboxed;
        let picked = pick_assignable_runner(&[sandboxed, free.clone()], &HashMap::new());
        let chosen = picked
            .as_ref()
            .expect("online native under cap is assignable");
        assert_eq!(chosen.id(), free.id);
        assert_eq!(chosen.host.isolation, RunnerIsolation::Native);

        // Local SANDBOX_BACKEND is available (test allow_local + plane taking).
        // Picker found a free remote native — must not local-spawn.
        for allow_local in [true, false] {
            for plane_taking in [true, false] {
                assert_eq!(
                    spawn_placement(picked.is_some(), allow_local, plane_taking, false),
                    SpawnPlacement::Remote,
                    "free remote native wins over local SANDBOX_BACKEND \
                     (allow_local={allow_local}, plane_taking={plane_taking})"
                );
            }
        }
        assert_ne!(
            spawn_placement(picked.is_some(), true, true, false),
            SpawnPlacement::Local,
            "ready task must not local-spawn when a free paired host exists"
        );

        let mut inflight = HashMap::new();
        inflight.insert(free.id, 4);
        assert!(
            pick_assignable_runner(&[free], &inflight).is_none(),
            "at-cap native is not free; picker must not invent a local steal"
        );
    }

    /// Docs §16: Settings git remote set after a run started — already-spawned
    /// sessions keep their workdir. New spawns clone the new URL.
    ///
    /// Locks the same sequence `schedule_and_spawn` uses: `apply_spawn_env`
    /// then `bind_spawn_git_remote` then `write_session_context` then the
    /// remote `spawn_payload` (FLEET_REPO_URL + workspace.json + repo_url).
    #[test]
    fn already_spawned_keep_workdir_on_remote_change() {
        let dir = tempfile::tempdir().unwrap();
        let spawned = dir
            .path()
            .join("projects")
            .join("11111111-1111-1111-1111-111111111111")
            .join("sessions")
            .join("22222222-2222-2222-2222-222222222222");
        std::fs::create_dir_all(spawned.join("repo")).unwrap();
        std::fs::write(spawned.join("repo/KEEP.txt"), "from-first-clone").unwrap();
        std::fs::write(
            spawned.join("workspace.json"),
            r#"{"repo":{"url":"https://example.com/old.git","ref":"main","auth_via":"proxy"}}"#,
        )
        .unwrap();

        let old = crate::domain::ProjectGitRemote {
            url: "https://example.com/old.git".into(),
            git_ref: "main".into(),
        };
        let new = crate::domain::ProjectGitRemote {
            url: "https://example.com/new.git".into(),
            git_ref: "develop".into(),
        };
        let task = spawn_git_task();

        let mut spawned_req = sample_req();
        spawned_req.work_dir = spawned.clone();
        apply_spawn_env(
            &mut spawned_req,
            "http://orch.example:8080",
            1800,
            None,
            None,
        );
        let spawned_git = bind_spawn_git_remote(&mut spawned_req, Some(&new));
        assert_eq!(
            spawned_git, None,
            "already-spawned session must not be re-cloned when Settings git remote changes"
        );
        assert!(
            !spawned_req.env.contains_key("FLEET_REPO_URL"),
            "already-spawned session must not receive the new Settings URL as FLEET_REPO_URL: {:?}",
            spawned_req.env.get("FLEET_REPO_URL")
        );
        assert!(
            !spawned_req.env.contains_key("FLEET_GIT_REF"),
            "already-spawned session must not receive a new FLEET_GIT_REF: {:?}",
            spawned_req.env.get("FLEET_GIT_REF")
        );
        crate::orch::session_context::write_session_context(
            &spawned_req.work_dir,
            &task,
            1800,
            spawned_git.as_ref(),
        )
        .expect("session context");
        let spawned_ws: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(spawned.join("workspace.json")).unwrap())
                .unwrap();
        assert_eq!(
            spawned_ws["repo"]["url"], "https://example.com/old.git",
            "already-spawned workspace.json must keep the spawn-time remote, not the new Settings URL"
        );
        assert_eq!(
            spawned_ws["repo"]["ref"], "main",
            "already-spawned workspace.json must keep the spawn-time git ref"
        );
        assert_eq!(
            std::fs::read_to_string(spawned.join("repo/KEEP.txt")).unwrap(),
            "from-first-clone",
            "already-spawned session workdir must stay as cloned at spawn"
        );
        let host_id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();
        let spawned_payload = spawn_payload(
            &spawned_req,
            "http://orch.example:8080",
            "host",
            1800,
            host_id,
        );
        assert_eq!(
            spawned_payload.repo_url, None,
            "remote spawn_payload must not carry the new Settings URL for an already-attached session"
        );
        assert_eq!(
            spawned_payload.git_ref, None,
            "remote spawn_payload must not rewrite git_ref on an already-attached session"
        );
        assert!(
            !spawned_payload
                .env
                .get("FLEET_REPO_URL")
                .is_some_and(|u| u.contains("new.git")),
            "host env must not carry the new Settings URL for an already-spawned session: {:?}",
            spawned_payload.env.get("FLEET_REPO_URL")
        );

        // Re-tick with the old URL still must not touch repo/.
        let again = bind_spawn_git_remote(&mut spawned_req, Some(&old));
        assert_eq!(
            again, None,
            "re-tick of an already-attached session must not rewrite repo/ even with the old URL"
        );

        let fresh = dir
            .path()
            .join("projects")
            .join("11111111-1111-1111-1111-111111111111")
            .join("sessions")
            .join("33333333-3333-3333-3333-333333333333");
        std::fs::create_dir_all(&fresh).unwrap();
        let mut fresh_req = sample_req();
        fresh_req.work_dir = fresh.clone();
        apply_spawn_env(&mut fresh_req, "http://orch.example:8080", 1800, None, None);
        let fresh_git = bind_spawn_git_remote(&mut fresh_req, Some(&new));
        assert_eq!(
            fresh_git,
            Some(new.clone()),
            "new spawn must clone the current Settings git remote"
        );
        assert_eq!(
            fresh_req.env.get("FLEET_REPO_URL").map(String::as_str),
            Some("https://example.com/new.git"),
            "new spawn must snapshot the current Settings URL onto FLEET_REPO_URL"
        );
        assert_eq!(
            fresh_req.env.get("FLEET_GIT_REF").map(String::as_str),
            Some("develop"),
            "new spawn must snapshot the current Settings git ref"
        );
        crate::orch::session_context::write_session_context(
            &fresh_req.work_dir,
            &task,
            1800,
            fresh_git.as_ref(),
        )
        .expect("fresh session context");
        let fresh_ws: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(fresh.join("workspace.json")).unwrap())
                .unwrap();
        assert_eq!(
            fresh_ws["repo"]["url"], "https://example.com/new.git",
            "new spawn workspace.json must clone the current Settings URL, not the previous remote"
        );
        assert_eq!(fresh_ws["repo"]["ref"], "develop");
        let fresh_payload = spawn_payload(
            &fresh_req,
            "http://orch.example:8080",
            "host",
            1800,
            host_id,
        );
        assert_eq!(
            fresh_payload.repo_url.as_deref(),
            Some("https://example.com/new.git"),
            "new spawn payload must carry the current Settings git remote"
        );
        assert_eq!(fresh_payload.git_ref.as_deref(), Some("develop"));

        assert_eq!(
            std::fs::read_to_string(spawned.join("repo/KEEP.txt")).unwrap(),
            "from-first-clone",
            "deciding a new spawn must not rewrite an already-spawned workdir"
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &std::fs::read_to_string(spawned.join("workspace.json")).unwrap()
            )
            .unwrap()["repo"]["url"],
            "https://example.com/old.git",
            "new spawn must not rewrite an already-spawned workspace.json"
        );
    }

    /// Docs §16: Settings git remote cleared → new tasks skip clone
    /// (prompt-only / mock). Must not pass a leftover `FLEET_REPO_URL` or
    /// clone the previous remote.
    ///
    /// Locks the same sequence `schedule_and_spawn` uses: `apply_spawn_env`
    /// then `bind_spawn_git_remote` then `write_session_context` then the
    /// remote `spawn_payload`.
    #[test]
    fn new_spawn_skips_clone_when_remote_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let leftover_url = "https://example.com/previous.git";
        let leftover_ref = "old-branch";
        let previous = crate::domain::ProjectGitRemote {
            url: leftover_url.into(),
            git_ref: leftover_ref.into(),
        };
        let blank = crate::domain::ProjectGitRemote {
            url: "   ".into(),
            git_ref: leftover_ref.into(),
        };
        let task = spawn_git_task();
        let host_id = Uuid::parse_str("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee").unwrap();

        let fresh = dir
            .path()
            .join("projects")
            .join("11111111-1111-1111-1111-111111111111")
            .join("sessions")
            .join("33333333-3333-3333-3333-333333333333");
        std::fs::create_dir_all(&fresh).unwrap();
        let mut leftover_req = sample_req();
        leftover_req.work_dir = fresh.clone();
        leftover_req
            .env
            .insert("FLEET_REPO_URL".into(), leftover_url.into());
        leftover_req
            .env
            .insert("FLEET_GIT_REF".into(), leftover_ref.into());
        apply_spawn_env(
            &mut leftover_req,
            "http://orch.example:8080",
            1800,
            None,
            None,
        );
        let cleared = bind_spawn_git_remote(&mut leftover_req, None);
        assert_eq!(
            cleared, None,
            "cleared Settings git remote must skip clone for a new spawn"
        );
        assert!(
            !leftover_req.env.contains_key("FLEET_REPO_URL"),
            "new spawn must not pass leftover FLEET_REPO_URL after Settings remote is cleared: {:?}",
            leftover_req.env.get("FLEET_REPO_URL")
        );
        assert!(
            !leftover_req.env.contains_key("FLEET_GIT_REF"),
            "new spawn must not pass leftover FLEET_GIT_REF after Settings remote is cleared: {:?}",
            leftover_req.env.get("FLEET_GIT_REF")
        );
        assert_eq!(
            leftover_req.env.get("FLEET_HARNESS").map(String::as_str),
            Some("mock"),
            "cleared remote is prompt-only / mock, not a leftover clone"
        );
        crate::orch::session_context::write_session_context(
            &leftover_req.work_dir,
            &task,
            1800,
            cleared.as_ref(),
        )
        .expect("session context");
        assert!(
            leftover_req.work_dir.join("prompt.md").is_file(),
            "cleared remote must still write prompt-only session context"
        );
        assert!(
            !leftover_req.work_dir.join("repo").exists(),
            "cleared remote must not create a session repo/ checkout"
        );
        assert!(
            !leftover_req.work_dir.join("workspace.json").exists(),
            "cleared remote must not write workspace.json (driver would clone leftover repo.url)"
        );
        let task_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(leftover_req.work_dir.join("task.json")).unwrap(),
        )
        .unwrap();
        let leftover_in_task = task_json
            .get("repo_url")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(
            leftover_in_task.is_empty(),
            "cleared remote must not leak leftover repo_url into task.json: {task_json}"
        );
        let prompt = std::fs::read_to_string(leftover_req.work_dir.join("prompt.md")).unwrap();
        assert!(
            !prompt.contains(leftover_url),
            "prompt-only spawn must not name the previous remote: {prompt}"
        );
        let payload = spawn_payload(
            &leftover_req,
            "http://orch.example:8080",
            "host",
            1800,
            host_id,
        );
        assert_eq!(
            payload.repo_url, None,
            "remote spawn_payload must not carry leftover FLEET_REPO_URL after Settings clear"
        );
        assert_eq!(
            payload.git_ref, None,
            "remote spawn_payload must not carry leftover FLEET_GIT_REF after Settings clear"
        );
        assert!(
            !payload.env.contains_key("FLEET_REPO_URL"),
            "host env must not carry leftover FLEET_REPO_URL after Settings clear: {:?}",
            payload.env.get("FLEET_REPO_URL")
        );
        assert!(
            !payload.env.contains_key("FLEET_GIT_REF"),
            "host env must not carry leftover FLEET_GIT_REF after Settings clear: {:?}",
            payload.env.get("FLEET_GIT_REF")
        );
        let dumped = serde_json::to_string(&payload).expect("spawn_payload serializes");
        assert!(
            !dumped.contains(leftover_url) && !dumped.contains("previous.git"),
            "cleared remote must not leak the previous clone URL onto the host payload: {dumped}"
        );

        let mut blank_req = sample_req();
        blank_req.work_dir = fresh.join("blank");
        std::fs::create_dir_all(&blank_req.work_dir).unwrap();
        blank_req
            .env
            .insert("FLEET_REPO_URL".into(), leftover_url.into());
        apply_spawn_env(&mut blank_req, "http://orch.example:8080", 1800, None, None);
        let blank_git = bind_spawn_git_remote(&mut blank_req, Some(&blank));
        assert_eq!(
            blank_git, None,
            "whitespace-only Settings URL is a clear, not a leftover clone"
        );
        assert!(
            !blank_req.env.contains_key("FLEET_REPO_URL"),
            "whitespace Settings URL must drop leftover FLEET_REPO_URL: {:?}",
            blank_req.env.get("FLEET_REPO_URL")
        );

        let spawned = dir
            .path()
            .join("projects")
            .join("11111111-1111-1111-1111-111111111111")
            .join("sessions")
            .join("22222222-2222-2222-2222-222222222222");
        std::fs::create_dir_all(spawned.join("repo")).unwrap();
        std::fs::write(spawned.join("repo/KEEP.txt"), "from-first-clone").unwrap();
        std::fs::write(
            spawned.join("workspace.json"),
            r#"{"repo":{"url":"https://example.com/previous.git","ref":"old-branch","auth_via":"proxy"}}"#,
        )
        .unwrap();
        let mut spawned_req = sample_req();
        spawned_req.work_dir = spawned.clone();
        spawned_req
            .env
            .insert("FLEET_REPO_URL".into(), leftover_url.into());
        apply_spawn_env(
            &mut spawned_req,
            "http://orch.example:8080",
            1800,
            None,
            None,
        );
        let spawned_git = bind_spawn_git_remote(&mut spawned_req, None);
        assert_eq!(
            spawned_git, None,
            "already-spawned session after Settings clear must not be re-cloned"
        );
        assert!(
            !spawned_req.env.contains_key("FLEET_REPO_URL"),
            "already-spawned session after clear must not receive leftover FLEET_REPO_URL"
        );
        crate::orch::session_context::write_session_context(
            &spawned_req.work_dir,
            &task,
            1800,
            spawned_git.as_ref(),
        )
        .expect("already-spawned session context");
        assert_eq!(
            std::fs::read_to_string(spawned.join("repo/KEEP.txt")).unwrap(),
            "from-first-clone",
            "clearing Settings must not wipe an already-spawned workdir"
        );

        let control = dir
            .path()
            .join("projects")
            .join("11111111-1111-1111-1111-111111111111")
            .join("sessions")
            .join("44444444-4444-4444-4444-444444444444");
        std::fs::create_dir_all(&control).unwrap();
        let mut control_req = sample_req();
        control_req.work_dir = control;
        apply_spawn_env(
            &mut control_req,
            "http://orch.example:8080",
            1800,
            None,
            None,
        );
        let control_git = bind_spawn_git_remote(&mut control_req, Some(&previous));
        assert_eq!(
            control_git,
            Some(previous.clone()),
            "control: a still-set Settings remote must still snapshot (not a tautology that every bind is None)"
        );
        assert_eq!(
            control_req.env.get("FLEET_REPO_URL").map(String::as_str),
            Some(leftover_url),
            "control: still-set Settings remote must set FLEET_REPO_URL"
        );
    }

    fn spawn_git_task() -> crate::store::BoardTaskRow {
        crate::store::BoardTaskRow {
            id: Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap(),
            project_id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            run_id: Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
            plan_id: Uuid::parse_str("55555555-5555-5555-5555-555555555555").unwrap(),
            plan_task_id: "t1".into(),
            title: "work".into(),
            summary: String::new(),
            spec_md: "do it".into(),
            status: "ready".into(),
            depends_on: vec![],
            priority: 1,
            worker_profile: "coding".into(),
            acceptance: serde_json::json!([]),
            external_ref: None,
            timeout_s: None,
            max_retries: 0,
            attempt: 0,
            claimed_session_id: None,
            last_report_seq: 0,
            progress_pct: 0,
            block_reason: None,
            hitl_notes: None,
            hitl_artifacts: None,
            hitl_resume_requested_at: None,
            hitl_notified_at: None,
            hitl_resolved_at: None,
            failure_reason: None,
            artifacts: None,
            retry_not_before: None,
            assigned_at: None,
            started_at: None,
            terminal_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn local_spawn_only_if_plane_taking_jobs_when_no_remote() {
        assert!(
            pick_assignable_runner(&[], &HashMap::new()).is_none(),
            "empty pool is not a remote native"
        );
        assert_eq!(
            spawn_placement(false, true, true, false),
            SpawnPlacement::Local,
            "plane taking jobs + no remote → local SANDBOX_BACKEND"
        );
        assert_eq!(
            spawn_placement(false, true, false, false),
            SpawnPlacement::HoldReady,
            "plane off + no remote → no local spawn"
        );
        assert_eq!(
            spawn_placement(false, false, true, false),
            SpawnPlacement::HoldReady,
            "production never local-spawns when the picker found no host"
        );
    }

    /// Extra: picker-None + `PlaneRunner::default_for` + `spawn_placement`.
    /// The TickRunner lock is `plane_runner_tick.rs` (`tick_project`).
    #[test]
    fn local_spawn_when_plane_taking_no_remote_host() {
        let mut sandboxed = native_host(4);
        sandboxed.isolation = RunnerIsolation::Sandboxed;
        let at_cap = native_host(1);
        let mut inflight = HashMap::new();
        inflight.insert(at_cap.id, 1);
        let mut disallowed = native_host(4);
        disallowed.allow_host = false;
        disallowed.advertised_host = true;
        let cloud = AssignableHost {
            isolation: crate::store::RunnerKind::from_stored("cloud").isolation(),
            ..native_host(4)
        };

        for (label, picked) in [
            ("empty", pick_assignable_runner(&[], &HashMap::new())),
            (
                "sandboxed-only",
                pick_assignable_runner(&[sandboxed], &HashMap::new()),
            ),
            (
                "at-cap native",
                pick_assignable_runner(&[at_cap], &inflight),
            ),
            (
                "plane-disallowed",
                pick_assignable_runner(&[disallowed], &HashMap::new()),
            ),
            (
                "cloud placeholder",
                pick_assignable_runner(&[cloud], &HashMap::new()),
            ),
        ] {
            assert!(
                picked.is_none(),
                "{label} must not be an assignable remote host"
            );
        }

        let default_plane = fleet_db::PlaneRunner::default_for(Uuid::nil());
        assert!(
            default_plane.allow_host,
            "missing plane row defaults allow_host true"
        );
        assert!(
            default_plane.taking_native_jobs(),
            "default plane takes native jobs"
        );
        let mut plane_off = default_plane.clone();
        plane_off.allow_host = false;
        assert!(
            !plane_off.taking_native_jobs(),
            "stop taking jobs is allow_host false"
        );

        // with_local_spawn / build_app_state: local SANDBOX_BACKEND is on.
        let allow_local = true;
        assert_eq!(
            spawn_placement(
                false,
                allow_local,
                default_plane.taking_native_jobs(),
                false
            ),
            SpawnPlacement::Local,
            "default plane + no remote → local SANDBOX_BACKEND"
        );
        assert_eq!(
            spawn_placement(false, allow_local, plane_off.taking_native_jobs(), false),
            SpawnPlacement::HoldReady,
            "plane stop-taking-jobs + no remote → stay ready"
        );
        assert_ne!(
            spawn_placement(false, allow_local, plane_off.taking_native_jobs(), false),
            SpawnPlacement::Local,
            "plane off must not local-spawn"
        );
    }

    /// Catalog: Kill guest mid-swap; task stays ready, no local fallback.
    /// Guest is dead (not assignable). Host last heartbeat was
    /// `software_updating` inside the 2 min hold. Plane taking jobs +
    /// `with_local_spawn` must not start the plane process sandbox.
    #[test]
    fn hold_ready_mid_host_swap_does_not_fall_back_to_local() {
        let has_assignable = false;
        let updating = true;
        assert!(
            hold_ready_for_host_update(has_assignable, updating),
            "killed guest mid-swap must hold ready"
        );
        assert!(
            hold_ready_for_host_update(true, updating),
            "do not fail over to another host while home restarts"
        );

        for allow_local in [true, false] {
            for plane_taking in [true, false] {
                let place = spawn_placement(has_assignable, allow_local, plane_taking, updating);
                assert_eq!(
                    place,
                    SpawnPlacement::HoldReady,
                    "mid-swap hold wins over local SANDBOX_BACKEND \
                     (allow_local={allow_local}, plane_taking={plane_taking})"
                );
                assert_ne!(
                    place,
                    SpawnPlacement::Local,
                    "must not fall back to the plane process sandbox mid-swap \
                     (allow_local={allow_local}, plane_taking={plane_taking})"
                );
                assert_ne!(
                    place,
                    SpawnPlacement::Remote,
                    "killed guest is not assignable; must not enqueue remote mid-swap"
                );
            }
        }

        assert_eq!(
            spawn_placement(true, true, true, true),
            SpawnPlacement::HoldReady,
            "even an assignable sibling must not steal while home is swapping"
        );

        // Contrast: offline without software_updating is not this hold.
        // Today's local fallback (plane taking + allow_local) stays.
        assert!(!hold_ready_for_host_update(false, false));
        assert_eq!(
            spawn_placement(false, true, true, false),
            SpawnPlacement::Local,
            "offline without updating is not hold_ready; local fallback stays"
        );
    }

    /// Docs §8.3 step 9 / §15.2: schedule_and_spawn must treat
    /// spawning|healthy|stuck|killing as occupying a max_agents slot.
    /// If stuck/killing were omitted, the next tick would over-spawn.
    #[test]
    fn spawn_capacity_holds_stuck_and_killing() {
        let src = include_str!("spawn.rs");
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("spawn.rs production source");
        assert!(
            prod.contains("free_slots_from_db_statuses"),
            "schedule_and_spawn must use domain capacity so stuck/killing hold a slot"
        );
        assert!(
            prod.contains("snap.sessions.iter().map(|s| s.status.as_str())"),
            "spawn free slots must come from live session status strings"
        );
        assert!(
            prod.contains("if free <= 0"),
            "spawn loop must stop when stuck/killing fill max_agents"
        );
        assert!(
            !prod.contains("status == \"healthy\"") && !prod.contains("status == \"spawning\""),
            "must not count only healthy/spawning — stuck/killing hold the slot too"
        );

        // Same gate as schedule_and_spawn: session.status strings + max_agents.
        assert_eq!(
            free_slots_from_db_statuses(["stuck"], 1),
            0,
            "stuck must hold the slot so the tick does not over-spawn"
        );
        assert_eq!(
            free_slots_from_db_statuses(["killing"], 1),
            0,
            "killing must hold the slot so the tick does not over-spawn"
        );
        assert_eq!(
            free_slots_from_db_statuses(["spawning"], 1),
            0,
            "spawning must hold the slot"
        );
        assert_eq!(
            free_slots_from_db_statuses(["healthy"], 1),
            0,
            "healthy must hold the slot"
        );
        assert_eq!(
            free_slots_from_db_statuses(["stuck", "killing"], 2),
            0,
            "stuck+killing at max_agents=2 must not look like idle capacity"
        );
        assert_eq!(
            free_slots_from_db_statuses(["stuck", "killing", "exited", "failed_spawn"], 2),
            0,
            "terminal siblings must not cancel stuck/killing holds"
        );
        assert_eq!(
            free_slots_from_db_statuses(["exited", "failed_spawn"], 1),
            1,
            "only exited/failed_spawn free the slot"
        );

        // Simulate the spawn loop break: a full cap of stuck/killing means
        // zero new assignments even with ready tasks waiting.
        let mut free = free_slots_from_db_statuses(["stuck", "killing"], 2);
        let mut spawned = 0usize;
        for _ready_task in 0..4 {
            if free <= 0 {
                break;
            }
            spawned += 1;
            free -= 1;
        }
        assert_eq!(
            spawned, 0,
            "ready tasks must stay queued while stuck/killing hold every slot"
        );
    }

    /// Docs §8.4: extra ready tasks stay ready until a slot frees.
    /// `free_slots` math is not enough — `schedule_and_spawn` must honor it
    /// (break when free<=0; decrement on local and remote success).
    /// Distinct from plane-hold / host-at-cap: this is project `max_agents`.
    #[test]
    fn spawn_honors_free_slots_extra_ready_stay_ready() {
        let src = include_str!("spawn.rs");
        let prod = src
            .split("#[cfg(test)]")
            .next()
            .expect("spawn.rs production source");
        assert!(
            prod.contains("free_slots_from_db_statuses"),
            "schedule_and_spawn must compute free slots from live session statuses"
        );
        assert!(
            prod.contains("if free <= 0"),
            "spawn loop must stop when max_agents slots are full so a sibling stays ready"
        );
        // Successful spawn consumes the slot in this tick so a second ready
        // task cannot also begin_spawn_assignment.
        assert!(
            prod.matches("free -= 1").count() >= 2,
            "local and remote success paths must decrement free so a sibling stays ready"
        );

        // Same gate as schedule_and_spawn: two independent ready tasks,
        // empty board, max_agents=1 — first assignment consumes the slot.
        let mut free = free_slots_from_db_statuses(std::iter::empty::<&str>(), 1);
        assert_eq!(free, 1, "empty board at max_agents=1 has one slot");
        let mut spawned = 0usize;
        for _ready_task in 0..2 {
            if free <= 0 {
                break;
            }
            spawned += 1;
            free -= 1;
        }
        assert_eq!(
            spawned, 1,
            "max_agents=1 must spawn one of two independent ready tasks"
        );
        assert_eq!(free, 0, "the extra ready task must not take a second slot");

        // Slot held by the live session — extra stays ready (not failed_spawn).
        let mut free = free_slots_from_db_statuses(["healthy"], 1);
        let mut spawned = 0usize;
        for _ready_task in 0..1 {
            if free <= 0 {
                break;
            }
            spawned += 1;
            free -= 1;
        }
        assert_eq!(
            spawned, 0,
            "one live session at max_agents=1 leaves the extra ready task queued"
        );
        assert_eq!(
            free_slots_from_db_statuses(["spawning"], 1),
            0,
            "spawning holds the slot the same as healthy"
        );
        assert_eq!(
            free_slots_from_db_statuses(["exited"], 1),
            1,
            "slot frees when the live session exits"
        );
        assert_eq!(
            free_slots_from_db_statuses(["failed_spawn"], 1),
            1,
            "failed_spawn does not hold the slot — that is not how extras stay ready"
        );
    }
}

pub async fn apply_retry_after_failure(
    store: &Store,
    task_id: Uuid,
    attempt: i32,
    max_retries: i32,
    cancel_requested: bool,
    exit_reason: crate::domain::ExitReason,
) -> Result<(), TickError> {
    use crate::domain::{BoardTaskStatus, RetryAction, should_retry};

    // load approximate status as in_progress for policy
    let decision = should_retry(
        attempt,
        max_retries,
        exit_reason,
        cancel_requested,
        BoardTaskStatus::InProgress,
    );
    match decision.action {
        RetryAction::RetryWait => {
            let not_before = chrono::Utc::now() + chrono::Duration::seconds(10);
            store
                .set_task_retry_wait(task_id, not_before, decision.failure_reason)
                .await?;
        }
        RetryAction::Fail => {
            store
                .set_task_done_failed(
                    task_id,
                    decision.failure_reason.unwrap_or("max_retries_exceeded"),
                )
                .await?;
        }
        RetryAction::Cancel => {
            store.set_task_cancelled(task_id, "cancel").await?;
        }
        RetryAction::None => {}
    }
    Ok(())
}
