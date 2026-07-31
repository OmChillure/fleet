//! Step 5 — dependency readiness decisions for queued tasks.

use crate::domain::{evaluate_dependencies, BoardTaskStatus, DepAction, DepDecision};
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
    use crate::domain::FAILURE_DEPENDENCY_FAILED;
    use std::collections::HashMap;
    use BoardTaskStatus::*;

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
