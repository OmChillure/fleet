//! Step 3 helpers — which plans are eligible (pure filter).

use crate::domain::{WorkPlanStatus, WorkRunStatus};

#[derive(Debug, Clone)]
pub struct PlanMaterializeCandidate {
    pub plan_id: uuid::Uuid,
    pub plan_status: WorkPlanStatus,
    pub run_status: WorkRunStatus,
    pub cancel_requested: bool,
    pub already_materialized: bool,
}

/// K30 + Docs §15.4: accepted plan and run, no cancel, not yet materialized.
pub fn is_materialize_eligible(c: &PlanMaterializeCandidate) -> bool {
    !c.already_materialized
        && !c.cancel_requested
        && c.plan_status.is_accepted()
        && c.run_status == WorkRunStatus::Accepted
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn accepted_candidate() -> PlanMaterializeCandidate {
        PlanMaterializeCandidate {
            plan_id: Uuid::new_v4(),
            plan_status: WorkPlanStatus::Accepted,
            run_status: WorkRunStatus::Accepted,
            cancel_requested: false,
            already_materialized: false,
        }
    }

    /// K30 / Docs §8.3: accepted plan+run, cancel_requested_at IS NULL,
    /// not yet materialized. list_pending does not filter cancel or
    /// run_status, so this gate must refuse those rows.
    #[test]
    fn materialize_rejects_cancel_already_materialized_and_non_accepted_run() {
        let base = accepted_candidate();
        let rows = [
            (
                "accepted plan+run, no cancel, not materialized",
                base.clone(),
                true,
            ),
            (
                "cancel_requested",
                PlanMaterializeCandidate {
                    cancel_requested: true,
                    ..base.clone()
                },
                false,
            ),
            (
                "already_materialized",
                PlanMaterializeCandidate {
                    already_materialized: true,
                    ..base.clone()
                },
                false,
            ),
            (
                "run Active (post-first-plan)",
                PlanMaterializeCandidate {
                    run_status: WorkRunStatus::Active,
                    ..base.clone()
                },
                false,
            ),
        ];
        for (name, candidate, want) in rows {
            assert_eq!(
                is_materialize_eligible(&candidate),
                want,
                "{name}"
            );
        }

        for status in [
            WorkRunStatus::IntentReceived,
            WorkRunStatus::SourcesResolved,
            WorkRunStatus::Ingested,
            WorkRunStatus::Planning,
            WorkRunStatus::PlanReady,
            WorkRunStatus::Accepted,
            WorkRunStatus::Active,
            WorkRunStatus::Completed,
            WorkRunStatus::Failed,
            WorkRunStatus::Cancelled,
        ] {
            let want = match status {
                WorkRunStatus::Accepted => true,
                WorkRunStatus::IntentReceived
                | WorkRunStatus::SourcesResolved
                | WorkRunStatus::Ingested
                | WorkRunStatus::Planning
                | WorkRunStatus::PlanReady
                | WorkRunStatus::Active
                | WorkRunStatus::Completed
                | WorkRunStatus::Failed
                | WorkRunStatus::Cancelled => false,
            };
            assert_eq!(
                is_materialize_eligible(&PlanMaterializeCandidate {
                    run_status: status,
                    ..base.clone()
                }),
                want,
                "{status:?} run eligibility"
            );
        }
    }

    /// Docs §15.4: only Accepted is materialize-eligible.
    /// Ready/Superseded must stay ineligible here; the run-status sibling
    /// never plants plan_status.
    #[test]
    fn only_accepted_plans_materialize() {
        let accepted = accepted_candidate();
        for status in [
            WorkPlanStatus::Draft,
            WorkPlanStatus::Ready,
            WorkPlanStatus::Accepted,
            WorkPlanStatus::Superseded,
        ] {
            let want = matches!(status, WorkPlanStatus::Accepted);
            assert_eq!(
                is_materialize_eligible(&PlanMaterializeCandidate {
                    plan_status: status,
                    ..accepted.clone()
                }),
                want,
                "{status:?} plan eligibility"
            );
        }
    }
}
