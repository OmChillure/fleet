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
    use BoardTaskStatus::*;

    #[test]
    fn empty_deps_are_ready() {
        let d = evaluate_dependencies(&[]);
        assert_eq!(d.action, DepAction::Ready, "{d:?}");
        assert_eq!(d.failure_reason, None, "{d:?}");
    }

    #[test]
    fn all_success_deps_are_ready() {
        let d = evaluate_dependencies(&[DoneSuccess, DoneSuccess]);
        assert_eq!(d.action, DepAction::Ready, "{d:?}");
        assert_eq!(d.failure_reason, None, "{d:?}");
    }

    #[test]
    fn mixed_success_and_failed_dep_fails_with_dependency_failed() {
        let d = evaluate_dependencies(&[DoneSuccess, DoneFailed]);
        assert_eq!(d.action, DepAction::Fail, "{d:?}");
        assert_eq!(d.failure_reason, Some(FAILURE_DEPENDENCY_FAILED), "{d:?}");
    }

    #[test]
    fn cancelled_dep_sets_failure_reason_dependency_cancelled() {
        let d = evaluate_dependencies(&[Cancelled]);
        assert_eq!(d.action, DepAction::Cancel, "{d:?}");
        assert_eq!(
            d.failure_reason,
            Some(FAILURE_DEPENDENCY_CANCELLED),
            "{d:?}"
        );
    }

    #[test]
    fn any_cancelled_dep_cancels_even_if_sibling_succeeded_or_in_progress() {
        let with_success = evaluate_dependencies(&[DoneSuccess, Cancelled]);
        assert_eq!(with_success.action, DepAction::Cancel, "{with_success:?}");
        assert_eq!(
            with_success.failure_reason,
            Some(FAILURE_DEPENDENCY_CANCELLED),
            "{with_success:?}"
        );
        let with_in_progress = evaluate_dependencies(&[Cancelled, InProgress]);
        assert_eq!(
            with_in_progress.action,
            DepAction::Cancel,
            "{with_in_progress:?}"
        );
        assert_eq!(
            with_in_progress.failure_reason,
            Some(FAILURE_DEPENDENCY_CANCELLED),
            "{with_in_progress:?}"
        );
    }

    #[test]
    fn in_progress_dep_waits() {
        let d = evaluate_dependencies(&[DoneSuccess, InProgress]);
        assert_eq!(d.action, DepAction::Wait, "{d:?}");
        assert_eq!(d.failure_reason, None, "{d:?}");
    }

    #[test]
    fn done_failed_beats_cancelled() {
        let d = evaluate_dependencies(&[DoneFailed, Cancelled]);
        assert_eq!(d.action, DepAction::Fail, "{d:?}");
        assert_eq!(d.failure_reason, Some(FAILURE_DEPENDENCY_FAILED), "{d:?}");
    }

    #[test]
    fn nonterminal_dep_statuses_other_than_in_progress_wait() {
        let queued = evaluate_dependencies(&[DoneSuccess, Queued]);
        assert_eq!(queued.action, DepAction::Wait, "Queued {queued:?}");
        assert_eq!(queued.failure_reason, None, "Queued {queued:?}");
        let ready = evaluate_dependencies(&[DoneSuccess, Ready]);
        assert_eq!(ready.action, DepAction::Wait, "Ready {ready:?}");
        assert_eq!(ready.failure_reason, None, "Ready {ready:?}");
        let assigned = evaluate_dependencies(&[DoneSuccess, Assigned]);
        assert_eq!(assigned.action, DepAction::Wait, "Assigned {assigned:?}");
        assert_eq!(assigned.failure_reason, None, "Assigned {assigned:?}");
        let blocked = evaluate_dependencies(&[DoneSuccess, Blocked]);
        assert_eq!(blocked.action, DepAction::Wait, "Blocked {blocked:?}");
        assert_eq!(blocked.failure_reason, None, "Blocked {blocked:?}");
        let retry_wait = evaluate_dependencies(&[DoneSuccess, RetryWait]);
        assert_eq!(
            retry_wait.action,
            DepAction::Wait,
            "RetryWait {retry_wait:?}"
        );
        assert_eq!(retry_wait.failure_reason, None, "RetryWait {retry_wait:?}");
    }
}
