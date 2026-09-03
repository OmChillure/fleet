//! Step 5 — dependency readiness decisions for queued tasks.

use crate::domain::{BoardTaskStatus, DepAction, DepDecision, evaluate_dependencies};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct QueuedTaskView {
    pub id: Uuid,
    pub depends_on: Vec<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepUpdate {
    pub task_id: Uuid,
    pub decision: DepDecision,
}

/// For each queued task, look up dep statuses and produce a transition decision.
/// Only Ready / Fail / Cancel produce updates; Wait is omitted.
pub fn plan_dep_updates(
    queued: &[QueuedTaskView],
    status_by_id: &std::collections::HashMap<Uuid, BoardTaskStatus>,
) -> Vec<DepUpdate> {
    let mut out = Vec::new();
    for t in queued {
        let dep_stats: Vec<BoardTaskStatus> = t
            .depends_on
            .iter()
            .filter_map(|id| status_by_id.get(id).copied())
            .collect();
        // Missing dep ids: treat as wait (should not happen if materialize wired correctly)
        if dep_stats.len() != t.depends_on.len() {
            continue;
        }
        let d = evaluate_dependencies(&dep_stats);
        if d.action != DepAction::Wait {
            out.push(DepUpdate {
                task_id: t.id,
                decision: d,
            });
        }
    }
    out
}

pub fn dep_target_status(action: DepAction) -> Option<BoardTaskStatus> {
    match action {
        DepAction::Ready => Some(BoardTaskStatus::Ready),
        DepAction::Fail => Some(BoardTaskStatus::DoneFailed),
        DepAction::Cancel => Some(BoardTaskStatus::Cancelled),
        DepAction::Wait => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        FAILURE_DEPENDENCY_FAILED, MAX_DEPENDENCY_FAN_IN, PlanPolicy, PlanTask, WorkPlanBody,
        WorkPlanStatus,
    };
    use crate::validate::validate_plan;
    use BoardTaskStatus::*;
    use std::collections::HashMap;

    fn production_src(src: &'static str) -> &'static str {
        match src.find("#[cfg(test)]") {
            Some(i) => &src[..i],
            None => src,
        }
    }

    fn strip_line_comments(src: &str) -> String {
        src.lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
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

    fn first_live(src: &str, needle: &str) -> Option<usize> {
        strip_line_comments(src).find(needle)
    }

    fn must_live(src: &str, needle: &str, what: &str) -> usize {
        first_live(src, needle).unwrap_or_else(|| panic!("{what}:\n{src}"))
    }

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

    /// Docs §8.4: a 33-parent join or a cycle must not enter the store.
    /// `insert_accepted_plan` / `materialize_in_tx` are the only write paths;
    /// both must `validate_plan` before INSERT. `plan_dep_updates` only sees
    /// rows that already materialized.
    #[test]
    fn thirty_three_parent_join_or_cycle_must_not_enter_store() {
        assert_eq!(MAX_DEPENDENCY_FAN_IN, 32);

        let at_cap = join_plan(32);
        let at_deps = &at_cap.tasks.last().unwrap().depends_on;
        assert_eq!(at_deps.len(), 32);
        assert_eq!(
            at_deps
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            32,
            "32-parent fixture must be 32 distinct parents"
        );
        validate_plan(&at_cap).expect("32-parent join is at the cap and may enter the store");
        let over_plan = join_plan(33);
        let over_deps = &over_plan.tasks.last().unwrap().depends_on;
        assert_eq!(over_deps.len(), 33);
        assert_eq!(
            over_deps
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            33,
            "33-parent fixture must be 33 distinct parents, not 32 + a duplicate"
        );
        let over = validate_plan(&over_plan)
            .expect_err("33-parent join must not enter work_plans / board_tasks");
        let over_l = over.to_string().to_lowercase();
        assert!(
            (over_l.contains("fan-in") || over_l.contains("fan_in") || over_l.contains("fan in"))
                && over_l.contains("32"),
            "33-parent reject must name fan-in 32, got {over}"
        );

        let cycle = plan_with(vec![task("a", &["b"]), task("b", &["a"])]);
        let cycle_err =
            validate_plan(&cycle).expect_err("a cycle must not enter work_plans / board_tasks");
        assert!(
            cycle_err.to_string().to_lowercase().contains("cycle"),
            "cycle reject must name the cycle, got {cycle_err}"
        );

        let store_files = [
            (
                "store/mod.rs",
                production_src(include_str!("../store/mod.rs")),
            ),
            (
                "store/tick_ops.rs",
                production_src(include_str!("../store/tick_ops.rs")),
            ),
            (
                "store/host_pool.rs",
                production_src(include_str!("../store/host_pool.rs")),
            ),
            (
                "store/pool.rs",
                production_src(include_str!("../store/pool.rs")),
            ),
            (
                "store/models.rs",
                production_src(include_str!("../store/models.rs")),
            ),
        ];
        let mut plan_inserts = 0usize;
        let mut board_inserts = 0usize;
        for (name, src) in store_files {
            let live = strip_line_comments(src);
            let n_plans = live.matches("INSERT INTO work_plans").count();
            let n_board = live.matches("INSERT INTO board_tasks").count();
            if name != "store/mod.rs" {
                assert_eq!(
                    n_plans, 0,
                    "{name} must not INSERT work_plans (would skip fan-in/cycle reject):\n{src}"
                );
                assert_eq!(
                    n_board, 0,
                    "{name} must not INSERT board_tasks (would persist a smuggled DAG):\n{src}"
                );
            }
            plan_inserts += n_plans;
            board_inserts += n_board;
        }
        assert_eq!(
            plan_inserts, 1,
            "work_plans must have a single INSERT so a 33-parent/cycle cannot sneak in via a second writer"
        );
        assert_eq!(
            board_inserts, 1,
            "board_tasks must have a single INSERT (materialize_in_tx) so a smuggled DAG cannot persist"
        );

        let store_src = production_src(include_str!("../store/mod.rs"));
        let insert = fn_src(store_src, "pub async fn insert_accepted_plan");
        let validate_at = must_live(
            insert,
            "validate_plan(&body)",
            "insert_accepted_plan must call validate_plan so a 33-parent/cycle cannot persist",
        );
        let runs_at = must_live(
            insert,
            "INSERT INTO work_runs",
            "insert_accepted_plan must insert the run only after validation",
        );
        let plans_at = must_live(
            insert,
            "INSERT INTO work_plans",
            "insert_accepted_plan must insert the plan only after validation",
        );
        assert!(
            validate_at < runs_at && validate_at < plans_at,
            "validate_plan must reject before any INSERT; inserting first would store the illegal DAG:\n{insert}"
        );
        let insert_live = strip_line_comments(insert);
        assert!(
            !insert_live.contains("let _ = validate_plan")
                && !insert_live.contains("validate_plan(&body);"),
            "validate_plan result must fail the insert, not be ignored:\n{insert}"
        );
        assert!(
            insert_live.contains("validate_plan(&body).map_err")
                || insert_live.contains("validate_plan(&body)?"),
            "validate_plan failure must become StoreError and abort the insert:\n{insert}"
        );

        let materialize = fn_src(store_src, "async fn materialize_in_tx");
        let mat_validate = must_live(
            materialize,
            "validate_plan(&body)",
            "materialize_in_tx must re-validate so a smuggled 33-parent/cycle cannot become board_tasks",
        );
        let board_at = must_live(
            materialize,
            "INSERT INTO board_tasks",
            "materialize_in_tx must insert board_tasks only after validation",
        );
        assert!(
            mat_validate < board_at,
            "validate_plan must run before INSERT INTO board_tasks:\n{materialize}"
        );
        let mat_live = strip_line_comments(materialize);
        assert!(
            mat_live.contains("validate_plan(&body).map_err")
                || mat_live.contains("validate_plan(&body)?"),
            "materialize must not swallow validate_plan errors:\n{materialize}"
        );

        let handlers = production_src(include_str!("../api/handlers.rs"));
        let handlers_live = strip_line_comments(handlers);
        assert!(
            !handlers_live.contains("INSERT INTO work_plans")
                && !handlers_live.contains("INSERT INTO board_tasks")
                && !handlers_live.contains("INSERT INTO work_runs"),
            "HTTP handlers must not SQL-insert a plan (would skip fan-in/cycle reject):\n{handlers}"
        );
        let fixture = fn_src(handlers, "pub async fn fixture_run");
        let fixture_insert = must_live(
            fixture,
            "insert_accepted_plan(",
            "fixture_run is the HTTP write path and must go through insert_accepted_plan (which validates)",
        );
        assert!(
            fixture_insert < first_live(fixture, "Ok((").unwrap_or(fixture.len()),
            "fixture_run must not return CREATED before insert_accepted_plan:\n{fixture}"
        );

        let tick = production_src(include_str!("tick.rs"));
        let tick_live = strip_line_comments(tick);
        assert!(
            !tick_live.contains("INSERT INTO work_plans")
                && !tick_live.contains("INSERT INTO board_tasks"),
            "tick must not SQL-insert plans/tasks (would skip validate_plan):\n{tick}"
        );
        let step5_at = must_live(
            tick,
            "plan_dep_updates(&",
            "tick step 5 must use plan_dep_updates",
        );
        let materialize_at = must_live(
            tick,
            "store.materialize_plan",
            "tick must materialize (which re-validates) before dep updates",
        );
        assert!(
            materialize_at < step5_at,
            "plan_dep_updates only runs on the post-materialize snapshot; an illegal DAG must have been rejected already"
        );

        let validate_src = production_src(include_str!("../validate/mod.rs"));
        let gate = fn_src(validate_src, "pub fn validate_plan");
        let fan_in_at = must_live(
            gate,
            "depends_on.len() > MAX_DEPENDENCY_FAN_IN",
            "validate_plan must reject fan-in above 32 (`>` so 32 is allowed)",
        );
        let cycle_at = must_live(
            gate,
            "detect_cycle",
            "validate_plan must run cycle detection before Ok",
        );
        let ok_at = strip_line_comments(gate)
            .rfind("Ok(())")
            .unwrap_or_else(|| {
                panic!("validate_plan must return Ok only after both gates:\n{gate}")
            });
        assert!(
            fan_in_at < ok_at && cycle_at < ok_at,
            "fan-in and cycle gates must run before Ok so a 33-parent/cycle cannot be accepted:\n{gate}"
        );
        let validate_live = strip_line_comments(validate_src);
        assert!(
            validate_live.contains("Color::Gray")
                && validate_live.contains("dependency cycle involving"),
            "detect_cycle must DFS-reject back edges, not accept a cyclic depends_on"
        );
    }

    #[test]
    fn promotes_root_and_waits_on_child() {
        let root = Uuid::new_v4();
        let child = Uuid::new_v4();
        let queued = [
            QueuedTaskView {
                id: root,
                depends_on: vec![],
            },
            QueuedTaskView {
                id: child,
                depends_on: vec![root],
            },
        ];
        let mut map = HashMap::new();
        map.insert(root, Queued);
        map.insert(child, Queued);
        let updates = plan_dep_updates(&queued, &map);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].task_id, root);
        assert_eq!(updates[0].decision.action, DepAction::Ready);
    }

    #[test]
    fn fail_forward_when_dep_failed() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let queued = [QueuedTaskView {
            id: b,
            depends_on: vec![a],
        }];
        let mut map = HashMap::new();
        map.insert(a, DoneFailed);
        map.insert(b, Queued);
        let updates = plan_dep_updates(&queued, &map);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].decision.action, DepAction::Fail);
        assert_eq!(
            updates[0].decision.failure_reason,
            Some(FAILURE_DEPENDENCY_FAILED)
        );
        assert_eq!(
            dep_target_status(DepAction::Fail),
            Some(BoardTaskStatus::DoneFailed)
        );
    }
}
