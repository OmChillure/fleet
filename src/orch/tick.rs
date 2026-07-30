//! PR5a tick: lock → materialize → deps → rollup (no spawn yet).

use crate::domain::{BoardTaskStatus, OnTaskFailPolicy, WorkRunStatus};
use crate::orch::deps_step::{dep_target_status, plan_dep_updates, QueuedTaskView};
use crate::orch::materialize_step::{is_materialize_eligible, PlanMaterializeCandidate};
use crate::orch::rollup::{rollup_run, RollupAction, RunRollupInput};
use crate::store::Store;
use std::collections::HashMap;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Default, Clone)]
pub struct TickReport {
    pub tick_id: Uuid,
    pub plans_materialized: usize,
    pub tasks_materialized: usize,
    pub dep_updates: usize,
    pub runs_terminated: usize,
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
}

impl TickRunner {
    pub fn new(store: Store) -> Self {
        Self { store }
    }

    /// PR5a reconcile for one project (idempotent).
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
        // Step 3 — materialize eligible plans
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

        // Step 4 + 5 — snapshot and dependency readiness
        let snap = self.store.load_board_snapshot(project_id).await?;
        let mut status_by_id: HashMap<Uuid, BoardTaskStatus> = HashMap::new();
        for t in &snap.tasks {
            if let Some(s) = BoardTaskStatus::from_db(&t.status) {
                status_by_id.insert(t.id, s);
            }
        }

        let queued: Vec<QueuedTaskView> = snap
            .tasks
            .iter()
            .filter(|t| t.status == "queued")
            .map(|t| QueuedTaskView {
                id: t.id,
                depends_on: t.depends_on.clone(),
            })
            .collect();

        let updates = plan_dep_updates(&queued, &status_by_id);
        for u in updates {
            let Some(to) = dep_target_status(u.decision.action) else {
                continue;
            };
            let ok = self
                .store
                .update_task_status(
                    u.task_id,
                    "queued",
                    to.as_str(),
                    u.decision.failure_reason,
                )
                .await?;
            if ok {
                report.dep_updates += 1;
                status_by_id.insert(u.task_id, to);
            }
        }

        // Step 12 — rollup (reload tasks for accurate statuses after dep updates)
        let snap = self.store.load_board_snapshot(project_id).await?;
        let runs = self.store.list_runs_for_rollup(project_id).await?;
        let cfg = self.store.get_project_config(project_id).await?;
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

            let action = rollup_run(&RunRollupInput {
                run_status,
                cancel_requested: run.cancel_requested_at.is_some(),
                task_statuses,
                on_task_fail,
            });

            if let RollupAction::Terminal(term) = action {
                self.store
                    .set_run_terminal(run.id, term.as_db())
                    .await?;
                report.runs_terminated += 1;
            }
        }

        Ok(())
    }
}
