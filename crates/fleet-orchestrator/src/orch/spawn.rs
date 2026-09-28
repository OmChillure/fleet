//! Capacity schedule + spawn protocol.

use crate::domain::{WorkerSessionStatus, count_capacity_in_use, free_slots};
use crate::sandbox::remote::{self, RemoteHostHandle};
use crate::sandbox::{SpawnRequest, fleet_worker_env};
use crate::store::{
    BoardSnapshot, JobNeed, RunMode, Store, enqueue_remote_spawn, hold_ready_for_host_update,
    select_assignable_runner_preferring_for_job, workspace_has_updating_host,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

use super::tick::TickError;

pub struct SpawnOutcome {
    pub spawned: usize,
    pub spawn_failed: usize,
}

pub async fn schedule_and_spawn(
    store: &Store,
    project_id: Uuid,
    snap: &BoardSnapshot,
    max_agents: i32,
    data_dir: &PathBuf,
    orchestrator_url: &str,
    default_timeout_s: i32,
    fleet_llm: &str,
    fleet_llm_model: &str,
    integrations: crate::sandbox::io::IntegrationHandle,
) -> Result<SpawnOutcome, TickError> {
    let active_runs: HashSet<Uuid> = store
        .list_active_run_ids_not_cancelling(project_id)
        .await?
        .into_iter()
        .collect();

    let session_statuses: Vec<WorkerSessionStatus> = snap
        .sessions
        .iter()
        .filter_map(|s| WorkerSessionStatus::from_db(&s.status))
        .collect();
    let in_use = count_capacity_in_use(&session_statuses);
    let mut free = free_slots(max_agents, in_use);

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

    for task in ready {
        if free <= 0 {
            break;
        }
        // Missing worker_hosts must not fail the tick.
        let plan_hint = store.get_plan_body(task.plan_id).await.ok().flatten();
        let preferred = plan_hint.as_ref().and_then(|p| p.policy.preferred_host_id);
        let actor = plan_hint.as_ref().and_then(|p| p.policy.requested_by);
        let run_mode = plan_hint
            .as_ref()
            .and_then(|p| RunMode::parse(&p.policy.run_mode))
            .unwrap_or(RunMode::OnPc);
        let min_isolation =
            match fleet_db::repos::workspaces::min_isolation(store.pool(), project_id).await {
                Ok(raw) => crate::domain::IsolationClass::parse(&raw)
                    .unwrap_or(crate::domain::IsolationClass::Process),
                Err(_) => crate::domain::IsolationClass::Process,
            };
        let provider = plan_hint
            .as_ref()
            .and_then(|p| p.policy.llm_provider.clone());
        let vendor_key_ok = match run_mode {
            RunMode::OnPc => true,
            RunMode::InContainer => match provider.as_deref() {
                None | Some("mock") | Some("off") | Some("ollama") => true,
                Some(p) => fleet_db::repos::llm_keys::has_active(store.pool(), project_id, p)
                    .await
                    .unwrap_or(false),
            },
        };
        let job = JobNeed {
            run_mode,
            actor,
            min_isolation,
            provider,
            vendor_key_ok,
        };
        let remote_host = select_assignable_runner_preferring_for_job(
            store,
            project_id,
            preferred,
            orchestrator_url,
            &job,
        )
        .await;
        let updating = workspace_has_updating_host(store, project_id).await;
        if hold_ready_for_host_update(remote_host.is_some(), updating) {
            tracing::info!(
                task_id = %task.id,
                "paired host is applying a host-bin update; leaving task ready"
            );
            continue;
        }
        // Control plane is not a runner. No host → leave ready.
        let Some(chosen) = remote_host else {
            tracing::info!(
                task_id = %task.id,
                "no assignable host; leaving task ready"
            );
            continue;
        };
        let assignment = store
            .begin_spawn_assignment(
                task.id,
                project_id,
                task.run_id,
                &task.worker_profile,
                remote::BACKEND,
                None,
            )
            .await?;
        let Some(assignment) = assignment else {
            continue;
        };
        let retired = store
            .retire_live_sessions(task.id, Some(assignment.session_id), "superseded")
            .await?;
        signal_retired_sessions(store, project_id, &retired).await;

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
        let persona = crate::personas::resolve_for_workspace(
            store.pool(),
            project_id,
            &task.worker_profile,
            data_dir,
        )
        .await;
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
        req.env = fleet_worker_env(&req, orchestrator_url);
        req.env
            .insert("FLEET_TASK_TIMEOUT_S".into(), timeout_s.to_string());
        super::session_context::pass_through_driver_env(&mut req.env);
        // Guest is fleet-driver on the paired runner. Real models use the
        // official CLI (same family as chat_turn). Harness follows the
        // plan/chat provider (grok / claude / ollama). Unknown → mock.
        if req
            .env
            .get("FLEET_HARNESS")
            .map(|s| s.trim().is_empty())
            .unwrap_or(true)
        {
            let harness =
                crate::sandbox::agent_loop::harness_for_backend(req.agent_backend.as_deref());
            req.env.insert("FLEET_HARNESS".into(), harness.into());
        }
        if req
            .agent_model
            .as_deref()
            .map(str::trim)
            .is_none_or(|s| s.is_empty())
        {
            let pref = persona.model_preference.trim();
            if !pref.is_empty() && !pref.eq_ignore_ascii_case("auto") {
                req.agent_model = Some(pref.to_string());
            }
        }
        if let Some(model) = req.agent_model.as_deref().filter(|s| !s.is_empty()) {
            req.env
                .entry("FLEET_MODEL".into())
                .or_insert_with(|| model.to_string());
        }
        if let Some(effort) = plan_hint
            .as_ref()
            .and_then(|p| p.policy.reasoning_effort.clone())
            .filter(|s| !s.trim().is_empty())
        {
            req.env
                .entry("FLEET_REASONING_EFFORT".into())
                .or_insert(effort);
        }
        if job.run_mode == RunMode::InContainer {
            inject_workspace_vendor_key(
                &mut req.env,
                store.pool(),
                project_id,
                job.provider.as_deref(),
            )
            .await;
        }
        if let Some(notes) = task.hitl_notes.as_ref() {
            req.env.insert("FLEET_HITL_NOTES".into(), notes.clone());
        }
        // Optional shell override (legacy; fleet-driver ignores this).
        if let Ok(cmd) = std::env::var("FLEET_WORK_CMD")
            && !cmd.trim().is_empty()
        {
            req.env.insert("FLEET_WORK_CMD".into(), cmd);
        }
        if let Err(e) =
            super::session_context::write_session_context(&req.work_dir, task, timeout_s, None)
        {
            tracing::warn!(
                error = %e,
                path = %req.work_dir.display(),
                "failed to write session task context"
            );
        }

        let work_dir_str = req.work_dir.to_string_lossy().into_owned();
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
        if let Ok(mode) =
            fleet_db::repos::workspaces::default_egress(store.pool(), project_id).await
        {
            if mode != "inherit" {
                req.env.insert("FLEET_EGRESS".into(), mode.clone());
                if mode == "allowlist" {
                    if let Ok(list) = fleet_db::repos::workspaces::default_egress_allowlist(
                        store.pool(),
                        project_id,
                    )
                    .await
                    {
                        if !list.trim().is_empty() {
                            req.env.insert("FLEET_EGRESS_ALLOWLIST".into(), list);
                        }
                    }
                }
            }
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
    }
    Ok(outcome)
}

fn worker_url_is_loopback(url: &str) -> bool {
    agent_fleet_contracts::url_is_loopback(url)
}

pub async fn signal_retired_sessions(
    store: &Store,
    project_id: Uuid,
    sessions: &[crate::store::WorkerSessionRow],
) {
    for s in sessions {
        if s.runtime_backend != remote::BACKEND {
            continue;
        }
        let Some(host_id) = remote::host_id_from_handle(s.runtime_handle.as_deref().unwrap_or(""))
        else {
            continue;
        };
        let _ = store.void_spawn_claims(s.id).await;
        let _ =
            crate::store::enqueue_signal_session(store, project_id, s.id, host_id, "kill").await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::agent_loop::harness_for_backend;

    #[test]
    fn harness_maps_runner_clis_and_defaults_mock() {
        assert_eq!(harness_for_backend(Some("grok")), "grok");
        assert_eq!(harness_for_backend(Some("xai")), "grok");
        assert_eq!(harness_for_backend(Some("claude")), "claude");
        assert_eq!(harness_for_backend(Some("ollama")), "ollama");
        assert_eq!(harness_for_backend(Some("off")), "mock");
        assert_eq!(harness_for_backend(None), "mock");
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

    if store.live_claim_is_other_attempt(task_id, attempt).await? {
        return Ok(());
    }

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

async fn inject_workspace_vendor_key(
    _env: &mut HashMap<String, String>,
    _pool: &sqlx::PgPool,
    _workspace_id: Uuid,
    _provider: Option<&str>,
) {
    // Vendor keys stay on the plane. Guests call the control-plane proxy.
}
