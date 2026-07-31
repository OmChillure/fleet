//! Step 3 helpers — which plans are eligible (pure filter).

use crate::domain::WorkRunStatus;

#[derive(Debug, Clone)]
pub struct PlanMaterializeCandidate {
    pub plan_id: uuid::Uuid,
    pub run_status: WorkRunStatus,
    pub cancel_requested: bool,
    pub already_materialized: bool,
}

/// K30: only accepted runs without cancel, not yet materialized.
pub fn is_materialize_eligible(c: &PlanMaterializeCandidate) -> bool {
    !c.already_materialized
        && !c.cancel_requested
        && c.run_status == WorkRunStatus::Accepted
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn eligibility_matrix() {
        let base = PlanMaterializeCandidate {
            plan_id: Uuid::new_v4(),
            run_status: WorkRunStatus::Accepted,
            cancel_requested: false,
            already_materialized: false,
        };
        assert!(is_materialize_eligible(&base));
        assert!(!is_materialize_eligible(&PlanMaterializeCandidate {
            cancel_requested: true,
            ..base.clone()
        }));
        assert!(!is_materialize_eligible(&PlanMaterializeCandidate {
            already_materialized: true,
            ..base.clone()
        }));
        assert!(!is_materialize_eligible(&PlanMaterializeCandidate {
            run_status: WorkRunStatus::Active,
            ..base
        }));
    }
}
