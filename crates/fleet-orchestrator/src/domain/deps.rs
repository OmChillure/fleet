use super::retry::{FAILURE_DEPENDENCY_CANCELLED, FAILURE_DEPENDENCY_FAILED};
use super::status::BoardTaskStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepAction {
    Ready,
    Wait,
    Fail,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepDecision {
    pub action: DepAction,
    pub failure_reason: Option<&'static str>,
}

/// Step 5 fail-forward (K17).
pub fn evaluate_dependencies(dep_statuses: &[BoardTaskStatus]) -> DepDecision {
    if dep_statuses.is_empty() {
        return DepDecision {
            action: DepAction::Ready,
            failure_reason: None,
        };
    }
    if dep_statuses.contains(&BoardTaskStatus::DoneFailed) {
        return DepDecision {
            action: DepAction::Fail,
            failure_reason: Some(FAILURE_DEPENDENCY_FAILED),
        };
    }
    if dep_statuses.contains(&BoardTaskStatus::Cancelled) {
        return DepDecision {
            action: DepAction::Cancel,
            failure_reason: Some(FAILURE_DEPENDENCY_CANCELLED),
        };
    }
    if dep_statuses
        .iter()
        .all(|s| *s == BoardTaskStatus::DoneSuccess)
    {
        return DepDecision {
            action: DepAction::Ready,
            failure_reason: None,
        };
    }
    DepDecision {
        action: DepAction::Wait,
        failure_reason: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        MAX_DEPENDENCY_FAN_IN, PlanPolicy, PlanTask, WorkPlanBody, WorkPlanStatus,
    };
    use crate::validate::validate_plan;
    use BoardTaskStatus::*;
    use uuid::Uuid;

    fn task(id: &str, deps: &[&str]) -> PlanTask {
        PlanTask {
            plan_task_id: id.into(),
            title: id.into(),
            summary: String::new(),
            spec_md: String::new(),
            depends_on: deps.iter().map(|s| (*s).to_string()).collect(),
            worker_profile: "coding".into(),
            acceptance: vec![],
            priority: 0,
            external_ref: None,
            timeout_s: None,
            max_retries: None,
        }
    }

    fn plan_with(tasks: Vec<PlanTask>) -> WorkPlanBody {
        WorkPlanBody {
            plan_id: Uuid::new_v4(),
            project_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            version: 1,
            status: WorkPlanStatus::Accepted,
            source: "test".into(),
            tasks,
            policy: PlanPolicy::default(),
        }
    }

    fn join_plan(parent_count: usize) -> WorkPlanBody {
        let mut tasks: Vec<PlanTask> = (0..parent_count)
            .map(|i| task(&format!("p{i}"), &[]))
            .collect();
        let deps: Vec<String> = (0..parent_count).map(|i| format!("p{i}")).collect();
        let dep_refs: Vec<&str> = deps.iter().map(String::as_str).collect();
        tasks.push(task("join", &dep_refs));
        plan_with(tasks)
    }

    /// Docs §8.4: `depends_on` is a DAG. Fan-in cap is 32. A 33-parent join
    /// or a cycle is rejected at insert (`validate_plan`) and must not enter
    /// the store. Constant-only asserts are not coverage — the fixtures are
    /// 32 unique parents (ok) and 33 unique parents / real cycles (err).
    #[test]
    fn fan_in_deps_cap_32_cycles_rejected() {
        use std::collections::HashSet;

        assert_eq!(
            MAX_DEPENDENCY_FAN_IN, 32,
            "§8.4 fan-in cap is 32, not a different constant"
        );

        validate_plan(&plan_with(vec![task("root", &[])]))
            .unwrap_or_else(|e| panic!("a root with no deps must be accepted: {e}"));

        let at_cap = join_plan(32);
        assert_eq!(at_cap.tasks.len(), 33, "32 parents + one join");
        let at_deps = &at_cap.tasks.last().unwrap().depends_on;
        assert_eq!(
            at_deps.len(),
            32,
            "join fan-in must be exactly 32 unique parents, not a smaller fixture"
        );
        let at_unique: HashSet<&str> = at_deps.iter().map(String::as_str).collect();
        assert_eq!(
            at_unique.len(),
            32,
            "32-parent fixture must not pad fan-in with duplicate ids"
        );
        validate_plan(&at_cap)
            .unwrap_or_else(|e| panic!("a 32-parent join is at the cap and must be accepted: {e}"));

        let over_plan = join_plan(33);
        let over_deps = &over_plan.tasks.last().unwrap().depends_on;
        assert_eq!(over_deps.len(), 33);
        let over_unique: HashSet<&str> = over_deps.iter().map(String::as_str).collect();
        assert_eq!(
            over_unique.len(),
            33,
            "33-parent fixture must be 33 distinct parents, not 32 + a duplicate"
        );
        let over = validate_plan(&over_plan)
            .expect_err("a 33-parent join must be rejected at insert and must not enter the store");
        let over_l = over.to_string().to_lowercase();
        assert!(
            over_l.contains("fan-in") || over_l.contains("fan_in") || over_l.contains("fan in"),
            "33-parent reject must name the fan-in cap, got {over}"
        );
        assert!(
            over_l.contains("32"),
            "33-parent reject must name the cap 32, got {over}"
        );

        let diamond = plan_with(vec![
            task("a", &[]),
            task("b", &[]),
            task("c", &["a", "b"]),
            task("d", &["a", "b"]),
        ]);
        validate_plan(&diamond)
            .unwrap_or_else(|e| panic!("a diamond DAG is acyclic and must be accepted: {e}"));

        let cycle = plan_with(vec![task("a", &["b"]), task("b", &["a"])]);
        let cycle_err = validate_plan(&cycle)
            .expect_err("a 2-cycle must be rejected at insert and must not enter the store");
        assert!(
            cycle_err.to_string().to_lowercase().contains("cycle"),
            "cycle reject must name the cycle, got {cycle_err}"
        );

        let self_cycle = plan_with(vec![task("a", &["a"])]);
        let self_err = validate_plan(&self_cycle)
            .expect_err("a self-cycle must be rejected at insert and must not enter the store");
        assert!(
            self_err.to_string().to_lowercase().contains("cycle"),
            "self-cycle reject must name the cycle, got {self_err}"
        );

        let three = plan_with(vec![
            task("a", &["c"]),
            task("b", &["a"]),
            task("c", &["b"]),
        ]);
        let three_err = validate_plan(&three)
            .expect_err("a 3-cycle must be rejected at insert and must not enter the store");
        assert!(
            three_err.to_string().to_lowercase().contains("cycle"),
            "3-cycle reject must name the cycle, got {three_err}"
        );

        let mixed = plan_with(vec![task("a", &["b"]), task("b", &["a"]), task("c", &[])]);
        let mixed_err = validate_plan(&mixed).expect_err(
            "a cycle plus an independent root is still a cycle and must not enter the store",
        );
        assert!(
            mixed_err.to_string().to_lowercase().contains("cycle"),
            "disconnected cycle must name the cycle, got {mixed_err}"
        );

        let mut at_cap_cycle = join_plan(32);
        at_cap_cycle.tasks[0].depends_on = vec!["join".into()];
        let cap_cycle_err = validate_plan(&at_cap_cycle).expect_err(
            "a legal fan-in that also has a back-edge is still a cycle and must not enter the store",
        );
        assert!(
            cap_cycle_err.to_string().to_lowercase().contains("cycle"),
            "32-parent + back-edge must name the cycle, got {cap_cycle_err}"
        );
    }

    #[test]
    fn evaluate_matrix() {
        assert_eq!(evaluate_dependencies(&[]).action, DepAction::Ready);
        assert_eq!(
            evaluate_dependencies(&[DoneSuccess, DoneSuccess]).action,
            DepAction::Ready
        );
        let d = evaluate_dependencies(&[DoneSuccess, DoneFailed]);
        assert_eq!(d.action, DepAction::Fail);
        assert_eq!(d.failure_reason, Some(FAILURE_DEPENDENCY_FAILED));
        let d = evaluate_dependencies(&[Cancelled]);
        assert_eq!(d.action, DepAction::Cancel);
        let d = evaluate_dependencies(&[DoneSuccess, InProgress]);
        assert_eq!(d.action, DepAction::Wait);
        let d = evaluate_dependencies(&[DoneFailed, Cancelled]);
        assert_eq!(d.action, DepAction::Fail);
    }
}
