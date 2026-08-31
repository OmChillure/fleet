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

    fn production_src(src: &'static str) -> &'static str {
        match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        }
    }

    fn fn_src<'a>(src: &'a str, sig: &str) -> &'a str {
        let start = src.find(sig).unwrap_or_else(|| panic!("missing {sig}"));
        let after = start + sig.len();
        let end = src[after..]
            .find("\n    pub async fn ")
            .or_else(|| src[after..].find("\npub async fn "))
            .or_else(|| src[after..].find("\npub fn "))
            .or_else(|| src[after..].find("\nasync fn "))
            .or_else(|| src[after..].find("\npub(crate) fn "))
            .or_else(|| src[after..].find("\nfn "))
            .map(|i| after + i)
            .unwrap_or(src.len());
        &src[start..end]
    }

    fn accepted_candidate() -> PlanMaterializeCandidate {
        PlanMaterializeCandidate {
            plan_id: Uuid::new_v4(),
            plan_status: WorkPlanStatus::Accepted,
            run_status: WorkRunStatus::Accepted,
            cancel_requested: false,
            already_materialized: false,
        }
    }

    #[test]
    fn eligibility_matrix() {
        let base = accepted_candidate();
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

    /// Docs §15.4: `draft` / `ready` / `accepted` / `superseded`.
    /// Only accepted plans materialize. A draft plan on disk must not
    /// create board tasks — list + materialize_in_tx must refuse, not
    /// only a helper the store never consults.
    #[test]
    fn only_accepted_plans_materialize() {
        let accepted = accepted_candidate();
        assert!(
            is_materialize_eligible(&accepted),
            "accepted plan + accepted run must materialize"
        );
        for status in [
            WorkPlanStatus::Draft,
            WorkPlanStatus::Ready,
            WorkPlanStatus::Superseded,
        ] {
            assert!(
                !is_materialize_eligible(&PlanMaterializeCandidate {
                    plan_status: status,
                    ..accepted.clone()
                }),
                "{status:?} plan must not materialize"
            );
        }

        let store_src = production_src(include_str!("../store/mod.rs"));
        let list = fn_src(store_src, "pub async fn list_pending_materialize_plans");
        assert!(
            list.contains("p.status = 'accepted'"),
            "list_pending_materialize_plans must not offer draft/ready/superseded:\n{list}"
        );
        assert!(
            list.contains("p.status AS plan_status"),
            "list_pending must select work_plans.status so tick can refuse a smuggled draft:\n{list}"
        );
        for forbidden in ["draft", "ready", "superseded"] {
            assert!(
                !list.contains(&format!("'{forbidden}'")),
                "list_pending must not include {forbidden} plans:\n{list}"
            );
        }

        let materialize = fn_src(store_src, "async fn materialize_in_tx");
        let plan_status_at = materialize.find("p.status").expect(
            "materialize_in_tx must load work_plans.status so a draft row cannot become board_tasks",
        );
        let board_at = materialize
            .find("INSERT INTO board_tasks")
            .expect("materialize_in_tx must insert board_tasks only after the plan-status gate");
        assert!(
            plan_status_at < board_at,
            "plan status must be read before INSERT INTO board_tasks:\n{materialize}"
        );
        let gate_at = [
            "plan_status != WorkPlanStatus::Accepted.as_db()",
            "plan.plan_status != WorkPlanStatus::Accepted.as_db()",
            "!WorkPlanStatus::from_db(&plan.plan_status).is_some_and(|s| s.is_accepted())",
            "plan_status != \"accepted\"",
        ]
        .into_iter()
        .filter_map(|needle| materialize.find(needle))
        .min()
        .expect(
            "materialize_in_tx must refuse non-accepted plan status before creating board tasks",
        );
        assert!(
            gate_at < board_at,
            "non-accepted plan status must abort before INSERT INTO board_tasks:\n{materialize}"
        );
        let abort_at = ["return Ok(0)", "return Err("]
            .into_iter()
            .filter_map(|needle| materialize[gate_at..].find(needle).map(|i| gate_at + i))
            .min()
            .expect(
                "non-accepted plan status must return before INSERT, not only be compared:\n{materialize}",
            );
        assert!(
            abort_at < board_at,
            "non-accepted plan must return before INSERT INTO board_tasks:\n{materialize}"
        );

        let tick = production_src(include_str!("tick.rs"));
        let eligible_at = tick.find("is_materialize_eligible(").expect(
            "tick must consult is_materialize_eligible so draft/ready/superseded cannot materialize",
        );
        let mat_at = tick
            .find("store.materialize_plan")
            .expect("tick must materialize only after the eligibility filter");
        assert!(
            eligible_at < mat_at,
            "tick must skip ineligible plans before materialize_plan"
        );
        assert!(
            tick.contains("WorkPlanStatus::from_db") && tick.contains("p.plan_status"),
            "tick must parse work_plans.status from the pending row, not hardcode Accepted:\n{tick}"
        );
        assert!(
            !tick.contains("plan_status: WorkPlanStatus::Accepted"),
            "tick must not treat every pending row as Accepted:\n{tick}"
        );
    }
}
