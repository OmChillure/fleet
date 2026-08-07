//! Full reconcile tick PR5a–c: lock → … → spawn → stuck → cancel → rollup → reap.
//! PR10: external board sync after status mutations when `auto_sync_external`.

use crate::domain::{BoardTaskStatus, OnTaskFailPolicy, WorkRunStatus};
use crate::orch::board_sync_step::sync_external_board;
use crate::orch::cancel::cancel_drain;
use crate::orch::deps_step::{dep_target_status, plan_dep_updates, QueuedTaskView};
use crate::orch::inbox::apply_inbox;
use crate::orch::materialize_step::{is_materialize_eligible, PlanMaterializeCandidate};
use crate::orch::reap::orphan_reap;
use crate::orch::rollup::{rollup_run, RollupAction, RunRollupInput};
use crate::orch::spawn::schedule_and_spawn;
use crate::orch::stuck::process_sessions;
use crate::sandbox::SandboxRuntime;
use crate::store::Store;
use crate::sync::{ExternalBoardSync, NoopBoardSync};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use uuid::Uuid;

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
    /// PR10: tasks successfully pushed to ExternalBoardSync this tick.
    pub external_synced: usize,
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
    runtime: Arc<dyn SandboxRuntime>,
    board_sync: Arc<dyn ExternalBoardSync>,
    data_dir: PathBuf,
    orchestrator_url: String,
}

impl TickRunner {
    pub fn new(
        store: Store,
        runtime: Arc<dyn SandboxRuntime>,
        data_dir: PathBuf,
        orchestrator_url: impl Into<String>,
    ) -> Self {
        Self::with_board_sync(
            store,
            runtime,
            Arc::new(NoopBoardSync),
            data_dir,
            orchestrator_url,
        )
    }

    pub fn with_board_sync(
        store: Store,
        runtime: Arc<dyn SandboxRuntime>,
        board_sync: Arc<dyn ExternalBoardSync>,
        data_dir: PathBuf,
        orchestrator_url: impl Into<String>,
    ) -> Self {
        Self {
            store,
            runtime,
            board_sync,
            data_dir,
            orchestrator_url: orchestrator_url.into(),
        }
    }

    /// Convenience for tests: stub runtime, temp data dir, noop board sync.
    pub fn with_store_and_runtime(store: Store, runtime: Arc<dyn SandboxRuntime>) -> Self {
        Self::new(
            store,
            runtime,
            std::env::temp_dir().join("fleet-orch-data"),
            "http://127.0.0.1:8080",
        )
    }

    pub async fn tick_project(&self, project_id: Uuid) -> Result<TickReport, TickError> {
        let tick_id = Uuid::new_v4();
        let mut report = TickReport {
            tick_id,
            ..Default::default()
        };

        let mut lock_conn = self.store.lock_project(project_id).await?;
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
        report.inbox_applied = apply_inbox(&self.store, &self.runtime, project_id).await?;

        // Step 1b — repair assigned+healthy
        let _ = self.store.repair_assigned_to_in_progress(project_id).await?;

        // Snapshot for cancel (need sessions)
        let mut snap = self.store.load_board_snapshot(project_id).await?;

        // Step 2 — cancel drain
        let c = cancel_drain(
            &self.store,
            &self.runtime,
            project_id,
            &snap,
            cfg.kill_grace_s,
        )
        .await?;
        report.cancel_tasks += c.tasks_cancelled;

        // Step 2b — HITL resume (skip if cancelling handled in SQL)
        let _ = self.store.apply_hitl_resume(project_id).await?;

        // Step 3 — materialize
        let pending = self.store.list_pending_materialize_plans(project_id).await?;
        for p in pending {
            let run_status = WorkRunStatus::from_db(&p.run_status)
                .ok_or_else(|| TickError::Other(format!("bad run status {}", p.run_status)))?;
            let candidate = PlanMaterializeCandidate {
                plan_id: p.plan_id,
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

        // Refresh for schedule
        snap = self.store.load_board_snapshot(project_id).await?;

        // Step 6–7 — spawn
        let spawn_out = schedule_and_spawn(
            &self.store,
            &self.runtime,
            project_id,
            &snap,
            cfg.max_agents,
            &self.data_dir,
            &self.orchestrator_url,
            cfg.default_task_timeout_s,
        )
        .await?;
        report.spawned = spawn_out.spawned;
        report.spawn_failed = spawn_out.spawn_failed;

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
            &self.runtime,
            project_id,
            &snap,
            cfg.heartbeat_stale_s,
            cfg.spawn_timeout_s,
            cfg.kill_grace_s,
            cfg.default_task_timeout_s,
            &cancel_by_run,
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
            }
        }

        // Step 13 — orphan reap
        snap = self.store.load_board_snapshot(project_id).await?;
        report.reaped = orphan_reap(&self.store, &self.runtime, project_id, &snap).await?;

        // Step 14 — external board sync (PR10), gated by project config
        snap = self.store.load_board_snapshot(project_id).await?;
        report.external_synced =
            sync_external_board(&self.board_sync, &snap, cfg.auto_sync_external).await;

        Ok(())
    }
}
