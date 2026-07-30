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
    if dep_statuses
        .iter()
        .any(|s| *s == BoardTaskStatus::DoneFailed)
    {
        return DepDecision {
            action: DepAction::Fail,
            failure_reason: Some(FAILURE_DEPENDENCY_FAILED),
        };
    }
    if dep_statuses
        .iter()
        .any(|s| *s == BoardTaskStatus::Cancelled)
    {
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
    use BoardTaskStatus::*;

    #[test]
    fn evaluate_matrix() {
        assert_eq!(
            evaluate_dependencies(&[]).action,
            DepAction::Ready
        );
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
