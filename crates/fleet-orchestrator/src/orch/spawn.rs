//! Step 6–7 capacity schedule + spawn protocol.

use crate::domain::{WorkerSessionStatus, count_capacity_in_use, free_slots};
use crate::sandbox::{SandboxRuntime, SpawnRequest, fleet_worker_env};
use crate::store::{BoardSnapshot, Store};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

use super::tick::TickError;

pub struct SpawnOutcome {
    pub spawned: usize,
    pub spawn_failed: usize,
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
        let assignment = store
            .begin_spawn_assignment(
                task.id,
                project_id,
                task.run_id,
                &task.worker_profile,
                runtime.backend_name(),
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
        crate::sandbox::share::ensure_run_workspace(
            data_dir,
            project_id,
            task.run_id,
            None,
            None,
        )
        .await;

        crate::sandbox::team::prepare_agent_workspace(
            data_dir,
            &session_work,
            task,
            snap,
        )
        .await;

        let timeout_s = task.timeout_s.unwrap_or(default_timeout_s).max(1) as u64;
        let persona = crate::personas::resolve(&task.worker_profile, data_dir).await;
        let llm = if let Some(cached) = plan_llms.get(&task.plan_id) {
            cached.clone()
        } else {
            let plan = store.get_plan_body(task.plan_id).await.ok().flatten();
            let resolved = crate::sandbox::agent_loop::resolve_worker_llm(
                plan.as_ref()
                    .and_then(|p| p.policy.llm_provider.as_deref()),
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
            agent_backend: llm.backend,
            agent_model: llm.model,
            io: integrations.clone(),
        };
        req.env = fleet_worker_env(&req, orchestrator_url);
        req.env
            .insert("FLEET_TASK_TIMEOUT_S".into(), timeout_s.to_string());
        super::session_context::pass_through_driver_env(&mut req.env);
        if let Some(notes) = task.hitl_notes.as_ref() {
            req.env.insert("FLEET_HITL_NOTES".into(), notes.clone());
        }
        // Optional real work payload for fleet-worker (shell command).
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
