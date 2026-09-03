//! Reconcile tick: lock → materialize → spawn → stuck → cancel → rollup → reap.

use crate::domain::{BoardTaskStatus, OnTaskFailPolicy, WorkPlanStatus, WorkRunStatus};
use crate::notify::{LogNotify, NotificationSink};
use crate::orch::board_sync_step::sync_external_board;
use crate::orch::cancel::cancel_drain;
use crate::orch::deps_step::{QueuedTaskView, dep_target_status, plan_dep_updates};
use crate::orch::inbox::apply_inbox;
use crate::orch::materialize_step::{PlanMaterializeCandidate, is_materialize_eligible};
use crate::orch::reap::orphan_reap;
use crate::orch::rollup::{RollupAction, RunRollupInput, rollup_run};
use crate::orch::spawn::schedule_and_spawn;
use crate::orch::stuck::process_sessions;
use crate::store::Store;
use crate::sync::{ExternalBoardSync, NoopBoardSync};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct KillSessionReport {
    pub session_id: Uuid,
    pub already_dead: bool,
    pub signaled: bool,
}

#[derive(Debug, Clone)]
pub struct ReplaceSessionReport {
    pub session_id: Uuid,
    pub task_id: Uuid,
    pub replaced: bool,
    pub already_dead: bool,
}

#[derive(Debug, Default, Clone)]
pub struct TickReport {
    pub tick_id: Uuid,
    pub plans_materialized: usize,
    pub tasks_materialized: usize,
    pub dep_updates: usize,
    pub inbox_applied: usize,
    pub spawned: usize,
    pub spawn_failed: usize,
    pub completed_via_runtime: usize,
    pub killed: usize,
    pub retried: usize,
    pub cancel_tasks: usize,
    pub retry_promoted: usize,
    pub reaped: usize,
    pub runs_terminated: usize,
    /// Tasks successfully pushed to ExternalBoardSync this tick.
    pub external_synced: usize,
    /// Control-plane write-backs (Slack / Notion / ticket comment) after a run ends.
    pub writebacks: usize,
}

#[derive(Debug, Error)]
pub enum TickError {
    #[error(transparent)]
    Store(#[from] crate::store::StoreError),
    #[error("{0}")]
    Other(String),
}

pub struct TickRunner {
    store: Store,
    board_sync: Arc<dyn ExternalBoardSync>,
    notify: Arc<dyn NotificationSink>,
    data_dir: PathBuf,
    orchestrator_url: String,
    integrations: crate::sandbox::io::IntegrationHandle,
}

impl TickRunner {
    pub fn new(store: Store, data_dir: PathBuf, orchestrator_url: impl Into<String>) -> Self {
        Self::with_board_sync(store, Arc::new(NoopBoardSync), data_dir, orchestrator_url)
    }

    pub fn with_board_sync(
        store: Store,
        board_sync: Arc<dyn ExternalBoardSync>,
        data_dir: PathBuf,
        orchestrator_url: impl Into<String>,
    ) -> Self {
        Self {
            store,
            board_sync,
            notify: Arc::new(LogNotify),
            data_dir,
            orchestrator_url: orchestrator_url.into(),
            integrations: crate::sandbox::io::IntegrationHandle::none(),
        }
    }

    /// Convenience for tests: temp data dir, noop board sync.
    pub fn with_store(store: Store) -> Self {
        Self::new(
            store,
            std::env::temp_dir().join("fleet-orch-data"),
            "http://127.0.0.1:8080",
        )
    }

    pub fn with_notify(mut self, notify: Arc<dyn NotificationSink>) -> Self {
        self.notify = notify;
        self
    }

    pub fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }

    /// Stop one live sandbox (UI / cookie kill). Signals the paired host,
    /// frees the claim, and cancels the board task.
    pub async fn kill_session(
        &self,
        project_id: Uuid,
        session_id: Uuid,
    ) -> Result<KillSessionReport, TickError> {
        let session = self
            .store
            .get_session(session_id)
            .await?
            .ok_or_else(|| TickError::Other("session not found".into()))?;
        if session.project_id != project_id {
            return Err(TickError::Other("session not found".into()));
        }
        if matches!(session.status.as_str(), "exited" | "failed_spawn") {
            return Ok(KillSessionReport {
                session_id,
                already_dead: true,
                signaled: false,
            });
        }

        let grace = chrono::Utc::now() + chrono::Duration::seconds(0);
        self.store
            .mark_session_killing(session.id, "ui_kill", grace)
            .await?;

        let mut signaled = false;
        let _ = self.store.void_spawn_claims(session.id).await;
        if session.runtime_backend == crate::sandbox::remote::BACKEND {
            if let Some(host_id) = crate::sandbox::remote::host_id_from_handle(
                session.runtime_handle.as_deref().unwrap_or(""),
            ) {
                if crate::store::enqueue_signal_session(
                    &self.store,
                    project_id,
                    session_id,
                    host_id,
                    "kill",
                )
                .await
                .is_ok()
                {
                    signaled = true;
                }
            }
        } else {
            let _ = self.store.complete_spawn_failure(session.id).await;
        }

        self.store
            .free_session_claim(session.board_task_id, session.id, "ui_kill")
            .await?;
        self.store
            .set_task_cancelled(session.board_task_id, "sandbox_killed")
            .await?;

        Ok(KillSessionReport {
            session_id,
            already_dead: false,
            signaled,
        })
    }

    /// Kill a live sandbox and put its board task back to `ready` so a new
    /// agent can pick it up. The workflow survives; the process does not.
    pub async fn replace_session(
        &self,
        project_id: Uuid,
        session_id: Uuid,
    ) -> Result<ReplaceSessionReport, TickError> {
        let session = self
            .store
            .get_session(session_id)
            .await?
            .ok_or_else(|| TickError::Other("session not found".into()))?;
        if session.project_id != project_id {
            return Err(TickError::Other("session not found".into()));
        }
        let task_id = session.board_task_id;
        if matches!(session.status.as_str(), "exited" | "failed_spawn") {
            // A later attempt may already own the task. Do not unclaim it.
            if !self.store.task_has_live_session(task_id).await? {
                let _ = self
                    .store
                    .reopen_task_for_changes(
                        task_id,
                        "worker_replaced",
                        "Previous worker already gone — task is ready for a new agent.",
                    )
                    .await;
            }
            return Ok(ReplaceSessionReport {
                session_id,
                task_id,
                replaced: false,
                already_dead: true,
            });
        }

        let retired = self
            .store
            .retire_live_sessions(task_id, None, "worker_replaced")
            .await?;
        super::spawn::signal_retired_sessions(&self.store, project_id, &retired).await;
        self.store
            .reopen_task_for_changes(
                task_id,
                "worker_replaced",
                "Worker replaced — task stays on the board for the next agent.",
            )
            .await?;

        Ok(ReplaceSessionReport {
            session_id,
            task_id,
            replaced: true,
            already_dead: false,
        })
    }

    /// Attach the control-plane integration proxy (fleet-server).
    pub fn with_integrations(
        mut self,
        inv: Arc<dyn crate::sandbox::io::IntegrationInvoker>,
    ) -> Self {
        self.integrations = crate::sandbox::io::IntegrationHandle::new(inv);
        self
    }

    pub async fn tick_project(&self, project_id: Uuid) -> Result<TickReport, TickError> {
        let tick_id = Uuid::new_v4();
        let mut report = TickReport {
            tick_id,
            ..Default::default()
        };

        let mut lock_conn = match self.store.lock_project(project_id).await {
            Err(crate::store::StoreError::Conflict(_)) => return Ok(report),
            other => other?,
        };
        let result = self.tick_project_locked(project_id, &mut report).await;
        let unlock = Store::unlock_project(&mut lock_conn, project_id).await;
        match (result, unlock) {
            (Ok(()), Ok(())) => Ok(report),
            (Ok(()), Err(e)) => Err(e.into()),
            (Err(e), _) => Err(e),
        }
    }

    async fn tick_project_locked(
        &self,
        project_id: Uuid,
        report: &mut TickReport,
    ) -> Result<(), TickError> {
        let cfg = self.store.get_project_config(project_id).await?;

        // Step 1 — inbox
        report.inbox_applied = apply_inbox(&self.store, project_id, &self.data_dir).await?;
        notify_new_blocks(&self.store, &self.notify, project_id).await;

        // Step 1b — repair assigned+healthy
        let _ = self
            .store
            .repair_assigned_to_in_progress(project_id)
            .await?;

        // Snapshot for cancel (need sessions)
        let mut snap = self.store.load_board_snapshot(project_id).await?;

        // Step 2 — cancel drain
        let c = cancel_drain(&self.store, project_id, &snap, cfg.kill_grace_s).await?;
        report.cancel_tasks += c.tasks_cancelled;

        // Step 2b — HITL resume (skip if cancelling handled in SQL)
        let _ = self.store.apply_hitl_resume(project_id).await?;

        // Step 3 — materialize
        let pending = self
            .store
            .list_pending_materialize_plans(project_id)
            .await?;
        for p in pending {
            let run_status = WorkRunStatus::from_db(&p.run_status)
                .ok_or_else(|| TickError::Other(format!("bad run status {}", p.run_status)))?;
            let plan_status = WorkPlanStatus::from_db(&p.plan_status)
                .ok_or_else(|| TickError::Other(format!("bad plan status {}", p.plan_status)))?;
            let candidate = PlanMaterializeCandidate {
                plan_id: p.plan_id,
                plan_status,
                run_status,
                cancel_requested: p.cancel_requested_at.is_some(),
                already_materialized: p.materialized_at.is_some(),
            };
            if !is_materialize_eligible(&candidate) {
                continue;
            }
            let n = self.store.materialize_plan(project_id, p.plan_id).await?;
            if n > 0 {
                report.plans_materialized += 1;
                report.tasks_materialized += n;
            }
        }

        // Step 4 — snapshot
        snap = self.store.load_board_snapshot(project_id).await?;
        let mut status_by_id: HashMap<Uuid, BoardTaskStatus> = HashMap::new();
        for t in &snap.tasks {
            if let Some(s) = BoardTaskStatus::from_db(&t.status) {
                status_by_id.insert(t.id, s);
            }
        }

        // Step 5 — deps
        let queued: Vec<QueuedTaskView> = snap
            .tasks
            .iter()
            .filter(|t| t.status == "queued")
            .map(|t| QueuedTaskView {
                id: t.id,
                depends_on: t.depends_on.clone(),
            })
            .collect();
        for u in plan_dep_updates(&queued, &status_by_id) {
            let Some(to) = dep_target_status(u.decision.action) else {
                continue;
            };
            if self
                .store
                .update_task_status(u.task_id, "queued", to.as_str(), u.decision.failure_reason)
                .await?
            {
                report.dep_updates += 1;
            }
        }

        // Step 5b — retry_wait → ready
        report.retry_promoted = self
            .store
            .promote_retry_wait(project_id, chrono::Utc::now())
            .await? as usize;

        snap = self.store.load_board_snapshot(project_id).await?;

        // Step 6–7 — spawn
        let spawn_out = schedule_and_spawn(
            &self.store,
            project_id,
            &snap,
            cfg.max_agents,
            &self.data_dir,
            &self.orchestrator_url,
            cfg.default_task_timeout_s,
            &cfg.fleet_llm,
            &cfg.fleet_llm_model,
            self.integrations.clone(),
        )
        .await?;
        report.spawned = spawn_out.spawned;
        report.spawn_failed = spawn_out.spawn_failed;
        snap = self.store.load_board_snapshot(project_id).await?;
        crate::sandbox::team::sync_team_boards(&self.data_dir, project_id, &snap).await;

        // Step 8 — stuck / stub completion / kill
        snap = self.store.load_board_snapshot(project_id).await?;
        let runs = self.store.list_runs_for_rollup(project_id).await?;
        let mut cancel_by_run = HashMap::new();
        for r in &runs {
            cancel_by_run.insert(r.id, r.cancel_requested_at.is_some());
        }
        // also load cancelling runs not in list
        for r in self.store.list_cancelling_runs(project_id).await? {
            cancel_by_run.insert(r.id, true);
        }

        let stuck_out = process_sessions(
            &self.store,
            project_id,
            &snap,
            cfg.heartbeat_stale_s,
            cfg.spawn_timeout_s,
            cfg.kill_grace_s,
            cfg.default_task_timeout_s,
            &cancel_by_run,
            &self.data_dir,
        )
        .await?;
        report.killed = stuck_out.killed;
        report.completed_via_runtime = stuck_out.completed_via_runtime;
        report.retried = stuck_out.retried;

        // Step 12 — rollup
        snap = self.store.load_board_snapshot(project_id).await?;
        let runs = self.store.list_runs_for_rollup(project_id).await?;
        let on_task_fail = match cfg.on_task_fail.as_str() {
            "leave" => OnTaskFailPolicy::Leave,
            _ => OnTaskFailPolicy::FailRun,
        };
        for run in runs {
            let run_status = WorkRunStatus::from_db(&run.status)
                .ok_or_else(|| TickError::Other(format!("bad run status {}", run.status)))?;
            if run_status.is_terminal() {
                continue;
            }
            let task_statuses: Vec<BoardTaskStatus> = snap
                .tasks
                .iter()
                .filter(|t| t.run_id == run.id)
                .filter_map(|t| BoardTaskStatus::from_db(&t.status))
                .collect();
            if let RollupAction::Terminal(term) = rollup_run(&RunRollupInput {
                run_status,
                cancel_requested: run.cancel_requested_at.is_some(),
                task_statuses,
                on_task_fail,
            }) {
                self.store.set_run_terminal(run.id, term.as_db()).await?;
                report.runs_terminated += 1;
                let ev = match term.as_db() {
                    "completed" => "run_completed",
                    "cancelled" => "run_cancelled",
                    _ => "run_failed",
                };
                let _ = self
                    .notify
                    .on_event(
                        ev,
                        serde_json::json!({
                            "project_id": project_id,
                            "run_id": run.id,
                            "status": term.as_db(),
                        }),
                    )
                    .await;
                report.writebacks += write_back_run(
                    &self.store,
                    &self.integrations,
                    &self.data_dir,
                    project_id,
                    run.id,
                    term.as_db(),
                    &snap,
                )
                .await;
            }
        }

        // Step 13 — orphan reap
        snap = self.store.load_board_snapshot(project_id).await?;
        crate::sandbox::team::sync_team_boards(&self.data_dir, project_id, &snap).await;
        report.reaped = orphan_reap(&self.store, project_id, &snap).await?;

        // External board sync, gated by project config
        snap = self.store.load_board_snapshot(project_id).await?;
        report.external_synced =
            sync_external_board(&self.board_sync, &snap, cfg.auto_sync_external).await;

        // Step 15 — wipe workdirs after a grace window (TEAM handoffs already archived).
        cleanup_terminal_session_dirs(&self.data_dir, project_id, &snap).await;

        Ok(())
    }
}

async fn write_back_run(
    store: &Store,
    io: &crate::sandbox::io::IntegrationHandle,
    data_dir: &std::path::Path,
    project_id: Uuid,
    run_id: Uuid,
    status: &str,
    snap: &crate::store::BoardSnapshot,
) -> usize {
    let Some(inv) = io.get() else {
        return 0;
    };
    let run_tasks: Vec<_> = snap.tasks.iter().filter(|t| t.run_id == run_id).collect();
    let plan_id = run_tasks.first().map(|t| t.plan_id);
    let plan = match plan_id {
        Some(id) => store.get_plan_body(id).await.ok().flatten(),
        None => None,
    };
    let summary = run_summary(data_dir, project_id, run_id, status, &run_tasks);
    let mut outs: Vec<crate::domain::PlanIo> = plan
        .as_ref()
        .map(|p| p.policy.outputs.clone())
        .unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    for t in &run_tasks {
        let Some(raw) = t.external_ref.as_ref() else {
            continue;
        };
        let Ok(ext) = serde_json::from_value::<crate::domain::ExternalRef>(raw.clone()) else {
            continue;
        };
        let tag = format!("{}:{}", ext.provider, ext.key);
        if !seen.insert(tag) {
            continue;
        }
        if let Some(io) = crate::sandbox::io::default_output_for_ref(&ext, &summary) {
            // Explicit plan.outputs win for the same provider+op; still reply on source.
            outs.push(io);
        }
    }
    let mut n = 0usize;
    for out in outs {
        let mut args =
            crate::sandbox::io::subst_output_args(&out.args, &summary, &run_id.to_string(), status);
        if out.provider == "notion" && out.operation == "create_page" {
            fill_notion_parent(inv.as_ref(), project_id, &mut args).await;
        }
        let res = inv
            .invoke(
                project_id,
                out.provider.clone(),
                out.operation.clone(),
                args,
            )
            .await;
        if res.ok {
            n += 1;
        } else {
            tracing::warn!(
                run_id = %run_id,
                provider = %out.provider,
                operation = %out.operation,
                error = %res.summary,
                "run write-back failed"
            );
        }
    }
    n
}

async fn fill_notion_parent(
    inv: &dyn crate::sandbox::io::IntegrationInvoker,
    project_id: Uuid,
    args: &mut serde_json::Value,
) {
    let has_parent = args
        .get("parent_page_id")
        .and_then(|v| v.as_str())
        .map(|s| !s.is_empty())
        .unwrap_or(false)
        || args
            .get("parent_database_id")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
    if has_parent {
        return;
    }
    let search = inv
        .invoke(
            project_id,
            "notion".into(),
            "search".into(),
            serde_json::json!({
                "query": "",
                "page_size": 5,
                "filter": { "property": "object", "value": "page" }
            }),
        )
        .await;
    if !search.ok {
        return;
    }
    let id = search
        .data
        .get("items")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|it| it.get("id").and_then(|v| v.as_str()))
        .find(|s| !s.is_empty())
        .map(str::to_string);
    if let Some(id) = id {
        args["parent_page_id"] = serde_json::json!(id);
    }
}

fn run_summary(
    data_dir: &std::path::Path,
    project_id: Uuid,
    run_id: Uuid,
    status: &str,
    tasks: &[&crate::store::BoardTaskRow],
) -> String {
    let mut lines = vec![format!("Fleet run {run_id} → {status}.")];
    for t in tasks.iter().take(12) {
        lines.push(format!("- {} [{}]: {}", t.plan_task_id, t.status, t.title));
    }
    let result =
        crate::sandbox::share::run_workspace_dir(data_dir, project_id, run_id).join("result.md");
    if let Ok(body) = std::fs::read_to_string(&result) {
        let clip: String = body.chars().take(1200).collect();
        if !clip.trim().is_empty() {
            lines.push(String::new());
            lines.push(clip);
        }
    }
    lines.join("\n")
}

const WORKDIR_GRACE: chrono::Duration = chrono::Duration::minutes(15);

async fn cleanup_terminal_session_dirs(
    data_dir: &std::path::Path,
    project_id: Uuid,
    snap: &crate::store::BoardSnapshot,
) {
    let now = chrono::Utc::now();
    for session in &snap.sessions {
        if !matches!(session.status.as_str(), "exited" | "failed_spawn") {
            continue;
        }
        let terminal_at = session.exited_at.unwrap_or(session.created_at);
        if now.signed_duration_since(terminal_at) < WORKDIR_GRACE {
            continue;
        }
        let path = data_dir
            .join("projects")
            .join(project_id.to_string())
            .join("sessions")
            .join(session.id.to_string());
        let project_dir = data_dir.join("projects").join(project_id.to_string());
        crate::sandbox::git_isolate::detach_worktree(&project_dir, &path).await;
        if path.exists() {
            let _ = tokio::fs::remove_dir_all(&path).await;
        }
    }
}

async fn notify_new_blocks(store: &Store, notify: &Arc<dyn NotificationSink>, project_id: Uuid) {
    let Ok(tasks) = store.list_tasks_slim(project_id, None).await else {
        return;
    };
    for t in tasks {
        if t.status != "blocked" || t.hitl_notified_at.is_some() {
            continue;
        }
        let ev = crate::notify::HitlBlockedEvent {
            project_id,
            run_id: t.run_id,
            board_task_id: t.id,
            reason: t
                .block_reason
                .clone()
                .or(t.hitl_notes.clone())
                .unwrap_or_else(|| "blocked".into()),
        };
        if notify.on_blocked(ev).await.is_ok() {
            let _ = store.mark_hitl_notified(t.id).await;
        }
    }
}
