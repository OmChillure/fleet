//! Plan validation: deps, cycles, size limits.

use crate::domain::{
    MAX_DEPENDENCY_FAN_IN, MAX_PLAN_BODY_BYTES, MAX_SPEC_MD_BYTES, MAX_TASKS_PER_PLAN, WorkPlanBody,
};
use std::collections::{HashMap, HashSet};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PlanValidationError {
    #[error("{0}")]
    Message(String),
}

pub type Result<T> = std::result::Result<T, PlanValidationError>;

fn err(msg: impl Into<String>) -> PlanValidationError {
    PlanValidationError::Message(msg.into())
}

pub fn validate_plan(p: &WorkPlanBody) -> Result<()> {
    if p.plan_id == Uuid::nil() {
        return Err(err("plan_id is required"));
    }
    if p.project_id == Uuid::nil() {
        return Err(err("project_id is required"));
    }
    if p.run_id == Uuid::nil() {
        return Err(err("run_id is required"));
    }
    if p.version < 1 {
        return Err(err("version must be >= 1"));
    }
    if p.source.trim().is_empty() {
        return Err(err("source is required"));
    }
    if p.tasks.is_empty() {
        return Err(err("tasks must not be empty"));
    }
    if p.tasks.len() > MAX_TASKS_PER_PLAN {
        return Err(err(format!("tasks exceed max {MAX_TASKS_PER_PLAN}")));
    }

    let raw = serde_json::to_vec(p).map_err(|e| err(format!("marshal plan: {e}")))?;
    if raw.len() > MAX_PLAN_BODY_BYTES {
        return Err(err(format!(
            "plan body exceeds max {MAX_PLAN_BODY_BYTES} bytes"
        )));
    }

    let mut ids = HashSet::new();
    for (i, t) in p.tasks.iter().enumerate() {
        if t.plan_task_id.trim().is_empty() {
            return Err(err(format!("tasks[{i}].plan_task_id is required")));
        }
        if !ids.insert(t.plan_task_id.clone()) {
            return Err(err(format!(
                "duplicate plan_task_id \"{}\"",
                t.plan_task_id
            )));
        }
        if t.title.trim().is_empty() {
            return Err(err(format!("tasks[{i}].title is required")));
        }
        if t.worker_profile.trim().is_empty() {
            return Err(err(format!("tasks[{i}].worker_profile is required")));
        }
        if t.spec_md.len() > MAX_SPEC_MD_BYTES {
            return Err(err(format!(
                "tasks[{i}].spec_md exceeds max {MAX_SPEC_MD_BYTES} bytes"
            )));
        }
        if t.depends_on.len() > MAX_DEPENDENCY_FAN_IN {
            return Err(err(format!(
                "tasks[{i}].depends_on exceeds max fan-in {MAX_DEPENDENCY_FAN_IN}"
            )));
        }
    }

    for (i, t) in p.tasks.iter().enumerate() {
        for d in &t.depends_on {
            if !ids.contains(d) {
                return Err(err(format!(
                    "tasks[{i}].depends_on references unknown plan_task_id \"{d}\""
                )));
            }
        }
    }

    detect_cycle(&p.tasks)?;
    Ok(())
}

fn detect_cycle(tasks: &[crate::domain::PlanTask]) -> Result<()> {
    let mut graph: HashMap<&str, Vec<&str>> = HashMap::new();
    for t in tasks {
        graph.insert(
            t.plan_task_id.as_str(),
            t.depends_on.iter().map(|s| s.as_str()).collect(),
        );
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Color {
        White,
        Gray,
        Black,
    }
    let mut color: HashMap<&str, Color> = graph.keys().map(|k| (*k, Color::White)).collect();

    fn visit<'a>(
        n: &'a str,
        graph: &HashMap<&'a str, Vec<&'a str>>,
        color: &mut HashMap<&'a str, Color>,
    ) -> Result<()> {
        color.insert(n, Color::Gray);
        if let Some(deps) = graph.get(n) {
            for &d in deps {
                match color.get(d).copied().unwrap_or(Color::White) {
                    Color::Gray => {
                        return Err(err(format!("dependency cycle involving \"{n}\"")));
                    }
                    Color::White => visit(d, graph, color)?,
                    Color::Black => {}
                }
            }
        }
        color.insert(n, Color::Black);
        Ok(())
    }

    let nodes: Vec<&str> = graph.keys().copied().collect();
    for n in nodes {
        if color.get(n) == Some(&Color::White) {
            visit(n, &graph, &mut color)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{OnTaskFailPolicy, PlanPolicy, PlanTask, WorkPlanStatus};
    use uuid::Uuid;

    fn valid_plan() -> WorkPlanBody {
        WorkPlanBody {
            plan_id: Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap(),
            project_id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
            run_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
            version: 1,
            status: WorkPlanStatus::Accepted,
            source: "fixture".into(),
            tasks: vec![
                PlanTask {
                    plan_task_id: "t1".into(),
                    title: "Root".into(),
                    summary: String::new(),
                    spec_md: String::new(),
                    depends_on: vec![],
                    worker_profile: "coding".into(),
                    acceptance: vec![],
                    priority: 100,
                    external_ref: None,
                    timeout_s: None,
                    max_retries: Some(2),
                },
                PlanTask {
                    plan_task_id: "t2".into(),
                    title: "Child".into(),
                    summary: String::new(),
                    spec_md: String::new(),
                    depends_on: vec!["t1".into()],
                    worker_profile: "coding".into(),
                    acceptance: vec![],
                    priority: 50,
                    external_ref: None,
                    timeout_s: None,
                    max_retries: None,
                },
            ],
            policy: PlanPolicy {
                max_agents_override: None,
                on_task_fail: OnTaskFailPolicy::FailRun,
                auto_sync_external: false,
                ..Default::default()
            },
        }
    }

    #[test]
    fn missing_dependency() {
        let mut p = valid_plan();
        p.tasks[1].depends_on = vec!["missing".into()];
        let e = validate_plan(&p).unwrap_err().to_string();
        assert!(e.contains("depends_on"), "{e}");
    }

    #[test]
    fn duplicate_id() {
        let mut p = valid_plan();
        p.tasks.push(PlanTask {
            plan_task_id: "t1".into(),
            title: "Dup".into(),
            summary: String::new(),
            spec_md: String::new(),
            depends_on: vec![],
            worker_profile: "coding".into(),
            acceptance: vec![],
            priority: 0,
            external_ref: None,
            timeout_s: None,
            max_retries: None,
        });
        assert!(validate_plan(&p).is_err());
    }

    #[test]
    fn empty_tasks() {
        let mut p = valid_plan();
        p.tasks.clear();
        assert!(validate_plan(&p).is_err());
    }

    #[test]
    fn validate_plan_rejects_over_max_tasks() {
        let mut p = valid_plan();
        p.tasks = (0..=MAX_TASKS_PER_PLAN)
            .map(|i| PlanTask {
                plan_task_id: format!("task-{i}"),
                title: "T".into(),
                summary: String::new(),
                spec_md: String::new(),
                depends_on: vec![],
                worker_profile: "coding".into(),
                acceptance: vec![],
                priority: 0,
                external_ref: None,
                timeout_s: None,
                max_retries: None,
            })
            .collect();
        assert_eq!(p.tasks.len(), 1001);
        let e = validate_plan(&p).unwrap_err().to_string();
        assert!(e.contains("1000"), "{e}");
    }

    #[test]
    fn validate_plan_accepts_exactly_max_tasks() {
        let mut p = valid_plan();
        p.tasks = (0..MAX_TASKS_PER_PLAN)
            .map(|i| PlanTask {
                plan_task_id: format!("task-{i}"),
                title: "T".into(),
                summary: String::new(),
                spec_md: String::new(),
                depends_on: vec![],
                worker_profile: "coding".into(),
                acceptance: vec![],
                priority: 0,
                external_ref: None,
                timeout_s: None,
                max_retries: None,
            })
            .collect();
        assert_eq!(p.tasks.len(), 1000);
        validate_plan(&p).unwrap();
    }

    #[test]
    fn max_spec_md() {
        let mut p = valid_plan();
        p.tasks[0].spec_md = "x".repeat(MAX_SPEC_MD_BYTES + 1);
        assert!(validate_plan(&p).is_err());
    }

    #[test]
    fn missing_title() {
        let mut p = valid_plan();
        p.tasks[0].title.clear();
        assert!(validate_plan(&p).is_err());
    }

    #[test]
    fn body_size() {
        let mut p = valid_plan();
        p.tasks[0].summary = "y".repeat(MAX_PLAN_BODY_BYTES);
        assert!(validate_plan(&p).is_err());
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
        let mut p = valid_plan();
        p.tasks = tasks;
        p
    }

    #[test]
    fn validate_plan_accepts_diamond_dag() {
        let p = plan_with(vec![
            task("a", &[]),
            task("b", &[]),
            task("c", &["a", "b"]),
            task("d", &["a", "b"]),
        ]);
        validate_plan(&p).unwrap();
    }

    #[test]
    fn validate_plan_rejects_three_cycle() {
        let p = plan_with(vec![
            task("a", &["c"]),
            task("b", &["a"]),
            task("c", &["b"]),
        ]);
        assert!(validate_plan(&p).is_err());
    }

    #[test]
    fn validate_plan_rejects_cycle_plus_independent_root() {
        let p = plan_with(vec![
            task("a", &["b"]),
            task("b", &["a"]),
            task("c", &[]),
        ]);
        assert!(validate_plan(&p).is_err());
    }

    #[test]
    fn validate_plan_rejects_at_cap_join_with_back_edge() {
        let n = MAX_DEPENDENCY_FAN_IN;
        let mut tasks: Vec<PlanTask> = (0..n).map(|i| task(&format!("p{i}"), &[])).collect();
        let deps: Vec<String> = (0..n).map(|i| format!("p{i}")).collect();
        let dep_refs: Vec<&str> = deps.iter().map(String::as_str).collect();
        tasks.push(task("join", &dep_refs));
        tasks[0].depends_on = vec!["join".into()];
        assert!(validate_plan(&plan_with(tasks)).is_err());
    }

    #[test]
    fn validate_plan_accepts_fan_out_33_children() {
        let mut tasks = vec![task("root", &[])];
        for i in 0..=MAX_DEPENDENCY_FAN_IN {
            tasks.push(task(&format!("c{i}"), &["root"]));
        }
        validate_plan(&plan_with(tasks)).unwrap();
    }

    #[test]
    fn validate_plan_accepts_chain_longer_than_fan_in_cap() {
        let mut tasks = vec![task("t0", &[])];
        for i in 1..=40 {
            let pred = format!("t{}", i - 1);
            tasks.push(task(&format!("t{i}"), &[pred.as_str()]));
        }
        validate_plan(&plan_with(tasks)).unwrap();
    }

    #[test]
    fn validate_plan_fan_in_counts_depends_on_len_including_duplicate_ids() {
        let deps = vec!["p0"; MAX_DEPENDENCY_FAN_IN + 1];
        let p = plan_with(vec![task("p0", &[]), task("join", &deps)]);
        assert_eq!(p.tasks[1].depends_on.len(), MAX_DEPENDENCY_FAN_IN + 1);
        assert!(validate_plan(&p).is_err());
    }
}
